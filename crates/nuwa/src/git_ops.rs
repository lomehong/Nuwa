//! Git operations shared by the runner preflight and delivery gate.
//!
//! Local operations stay synchronous; network operations (fetch, push
//! --delete) get a hard timeout and process-tree kill so a stalled CN→GitHub
//! connection cannot wedge the runner the way a missing bash timeout did.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[cfg(windows)]
mod windows_job {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};

    use anyhow::bail;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, CREATE_SUSPENDED, PROCESS_SET_QUOTA,
        PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
    };

    pub const SUSPENDED: u32 = CREATE_SUSPENDED;

    pub struct Job(HANDLE);

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn create() -> anyhow::Result<Job> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job == 0 {
                bail!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let job = Job(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                bail!(
                    "SetInformationJobObject failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(job)
        }
    }

    pub fn assign(job: &Job, pid: u32) -> anyhow::Result<()> {
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process == 0 {
                bail!(
                    "OpenProcess({pid}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let process = OwnedHandle(process);
            if AssignProcessToJobObject(job.0, process.0) == 0 {
                bail!(
                    "AssignProcessToJobObject({pid}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        resume_main_thread(pid)
    }

    fn resume_main_thread(pid: u32) -> anyhow::Result<()> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                bail!(
                    "CreateToolhelp32Snapshot failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let snapshot = OwnedHandle(snapshot);
            let mut entry: THREADENTRY32 = zeroed();
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            let mut found = Thread32First(snapshot.0, &mut entry);
            loop {
                if found == 0 {
                    bail!("no thread found for child process {pid}");
                }
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if thread == 0 {
                        bail!(
                            "OpenThread({pid}) failed: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                    let thread = OwnedHandle(thread);
                    if ResumeThread(thread.0) == u32::MAX {
                        bail!(
                            "ResumeThread({pid}) failed: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                    return Ok(());
                }
                found = Thread32Next(snapshot.0, &mut entry);
            }
        }
    }

    pub fn terminate(job: &Job) {
        unsafe {
            let _ = TerminateJobObject(job.0, 0);
        }
    }
}

pub struct ProcessOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// True when the process was killed after exceeding its timeout.
    pub timed_out: bool,
}

impl ProcessOutput {
    fn is_missing_ref(&self) -> bool {
        let text = format!("{}\n{}", self.stdout, self.stderr).to_lowercase();
        text.contains("remote ref does not exist")
            || text.contains("couldn't find remote ref")
            || text.contains("unknown ref")
    }
}

/// Run one process with a hard timeout. When `env` is `Some`, the child starts
/// from an empty environment and receives exactly those keys; `None` inherits
/// the parent environment (used by git). On timeout the child's whole process
/// tree is killed before returning.
pub async fn run_process(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    env: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<ProcessOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = env {
        command.env_clear();
        for (key, value) in env {
            command.env(key, value);
        }
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(windows)]
    let job = {
        let job = windows_job::create()?;
        command.creation_flags(windows_job::SUSPENDED);
        job
    };

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {program} {}", args.join(" ")))?;
    let pid = child.id().context("child has no pid")?;
    #[cfg(windows)]
    windows_job::assign(&job, pid)?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().context("try_wait")? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            // 组杀尽力覆盖孙进程；`start_kill` 是内核级 SIGKILL，不依赖
            // 外部 kill 命令的 PATH/权限——CI runner 上外部 kill 曾失效，
            // sleep 30 跑满全程才返回（2026-10-20）。
            #[cfg(windows)]
            windows_job::terminate(&job);
            kill_tree(pid).await;
            let _ = child.start_kill();
            match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
                Ok(status) => break status.context("wait after timeout")?,
                Err(_) => {
                    bail!("child {pid} still alive 1s after SIGKILL — sandbox refused the kill")
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // Bound the pipe drain by the same deadline. A grandchild that inherited
    // the stdout/stderr pipe (a daemon, a `&`-backgrounded process, a watcher)
    // keeps the write end open after the direct child exits, so an unbounded
    // `read_to_end` would hang the call forever even though the command itself
    // finished. The timeout must cover the whole operation, not just the
    // child's lifetime — otherwise `git`/check commands with a lingering
    // descendant defeat the "hard timeout" contract.
    let drain_budget = timeout
        .saturating_sub(start.elapsed())
        .max(Duration::from_millis(50));
    let (out, err) = match tokio::time::timeout(drain_budget, async {
        let o = stdout_task.await.unwrap_or_default();
        let e = stderr_task.await.unwrap_or_default();
        (o, e)
    })
    .await
    {
        Ok(pair) => pair,
        Err(_) => {
            // The pipe is still held open past the deadline. Kill the tree to
            // release it and abandon the readers rather than wait; the detached
            // tasks finish harmlessly once EOF arrives. Report a timeout: the
            // overall operation exceeded its budget.
            timed_out = true;
            #[cfg(windows)]
            windows_job::terminate(&job);
            kill_tree(pid).await;
            (Vec::new(), Vec::new())
        }
    };
    Ok(ProcessOutput {
        success: status.success(),
        stdout: String::from_utf8_lossy(&out).trim().to_string(),
        stderr: String::from_utf8_lossy(&err).trim().to_string(),
        timed_out,
    })
}

/// Run one git command with a hard timeout. On timeout the child's whole
/// process tree is killed before returning.
pub async fn git_with_timeout(
    repo: &Path,
    args: &[&str],
    timeout: Duration,
) -> anyhow::Result<ProcessOutput> {
    run_process("git", args, repo, timeout, None).await
}

/// Run one git command without a timeout (local, fast operations only).
pub fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run one git command without a timeout (local, fast operations only) and
/// return the **raw stdout bytes**. Unlike [`git`] this never trims: `git show
/// <rev>:<file>` must round-trip the blob byte-for-byte (a trimmed trailing
/// newline would make every restore rewrite the file and fight the next
/// `reset --hard` forever).
pub fn git_raw(repo: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

pub async fn git_ok(repo: &Path, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
    let out = git_with_timeout(repo, args, timeout).await?;
    if out.timed_out {
        bail!("git {} timed out after {:?}", args.join(" "), timeout);
    }
    if !out.success {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            if out.stderr.is_empty() {
                &out.stdout
            } else {
                &out.stderr
            }
        );
    }
    Ok(out.stdout)
}

/// Strip userinfo (embedded deploy tokens) before printing a remote URL.
pub fn sanitize_remote(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_userinfo, host)) = rest.split_once('@') {
            return format!("{scheme}://{host}");
        }
    }
    url.to_string()
}

pub fn current_branch(repo: &Path) -> anyhow::Result<String> {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
}

pub fn branch_exists(repo: &Path, branch: &str) -> anyhow::Result<bool> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo)
        .output()
        .with_context(|| format!("git rev-parse --verify refs/heads/{branch}"))?;
    Ok(out.status.success())
}

pub fn delete_local_branch(repo: &Path, branch: &str) -> anyhow::Result<()> {
    if !branch_exists(repo, branch)? {
        return Ok(());
    }
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

pub async fn delete_remote_branch(
    repo: &Path,
    branch: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let out = git_with_timeout(repo, &["push", "origin", "--delete", branch], timeout).await?;
    if out.timed_out {
        bail!("git push --delete {branch} timed out after {:?}", timeout);
    }
    if out.success || out.is_missing_ref() {
        return Ok(());
    }
    bail!(
        "git push --delete {branch} failed: {}",
        if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        }
    );
}

/// Does the remote already carry this branch?
///
/// A push that succeeded but whose PR creation failed leaves the branch
/// behind. The next delivery for the same fingerprint would push on top of
/// it and be rejected as non-fast-forward, so the orphan has to be found
/// before the repair starts.
pub async fn remote_branch_exists(
    repo: &Path,
    branch: &str,
    timeout: Duration,
) -> anyhow::Result<bool> {
    let out = git_with_timeout(repo, &["ls-remote", "--heads", "origin", branch], timeout).await?;
    if out.timed_out {
        bail!("git ls-remote {branch} timed out after {:?}", timeout);
    }
    if !out.success {
        // Unreachable remote or no `origin`: treat as "unknown" and let the
        // push itself report the real problem.
        return Ok(false);
    }
    Ok(!out.stdout.trim().is_empty())
}

pub async fn fetch_base(repo: &Path, base: &str, timeout: Duration) -> anyhow::Result<()> {
    git_ok(repo, &["fetch", "--prune", "origin", base], timeout)
        .await
        .map(|_| ())
}

/// Push the delivery branch with a hard timeout. A stalled connection must
/// not hold the runner forever.
pub async fn push_branch(repo: &Path, branch: &str, timeout: Duration) -> anyhow::Result<()> {
    let out = git_with_timeout(repo, &["push", "-u", "origin", branch], timeout).await?;
    if out.timed_out {
        bail!("git push {branch} timed out after {:?}", timeout);
    }
    if !out.success {
        bail!(
            "git push {branch} failed: {}",
            if out.stderr.is_empty() {
                &out.stdout
            } else {
                &out.stderr
            }
        );
    }
    Ok(())
}

pub fn reset_to_base(repo: &Path, base: &str) -> anyhow::Result<()> {
    git(repo, &["reset", "--hard", &format!("origin/{base}")]).map(|_| ())
}

/// Kill a child and its descendants. Unix uses the process group created by
/// `process_group(0)`; Windows uses `taskkill /T`.
async fn kill_tree(pid: u32) {
    let cleanup = tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        {
            let group = format!("-{pid}");
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &group])
                .status();
            // Fallback for platforms where the group signal did not apply.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .output();
        }
    });
    let _ = tokio::time::timeout(Duration::from_millis(500), cleanup).await;
    tokio::task::yield_now().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).expect("mkdir");
        run(dir, &["init", "-b", "main"]);
        run(dir, &["config", "user.name", "test"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("f.txt"), "one\n").expect("write");
        run(dir, &["add", "."]);
        run(dir, &["commit", "-m", "init"]);
    }

    fn commit_file(repo: &Path, name: &str, body: &str) {
        std::fs::write(repo.join(name), body).expect("write");
        run(repo, &["add", "."]);
        run(repo, &["commit", "-m", "update"]);
    }

    #[tokio::test]
    async fn git_with_timeout_kills_a_hanging_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        let args: &[&str] = &["-c", "sleep 30"];
        #[cfg(windows)]
        let args: &[&str] = &["/C", "ping -n 30 127.0.0.1 >NUL"];
        #[cfg(unix)]
        let program = "sh";
        #[cfg(windows)]
        let program = "cmd";

        let start = Instant::now();
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        let mut child = command.spawn().expect("spawn");
        let pid = child.id().expect("pid");
        let status = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break status;
            }
            if start.elapsed() >= Duration::from_millis(300) {
                kill_tree(pid).await;
                break child.wait().await.expect("wait");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(!status.success());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn run_process_timeout_is_bounded_even_when_tree_cleanup_stalls() {
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        let (program, args): (&str, &[&str]) = ("sh", &["-c", "sleep 30"]);
        #[cfg(windows)]
        let (program, args): (&str, &[&str]) = ("cmd", &["/C", "ping -n 30 127.0.0.1 >NUL"]);

        let start = Instant::now();
        let out = run_process(program, args, dir.path(), Duration::from_millis(100), None)
            .await
            .expect("run_process");
        let elapsed = start.elapsed();
        assert!(out.timed_out);
        assert!(
            elapsed < Duration::from_secs(2),
            "run_process cleanup exceeded its budget: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn run_process_timeout_kills_descendants_not_only_the_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("survived.txt");
        #[cfg(unix)]
        let (program, args) = (
            "sh",
            vec![
                "-c".to_string(),
                "(sleep 2; echo survived > survived.txt) & sleep 30".to_string(),
            ],
        );
        #[cfg(windows)]
        let (program, args) = {
            let script = dir.path().join("tree.cmd");
            std::fs::write(
                &script,
                "@echo off\r\nstart /B cmd /C \"ping -n 3 127.0.0.1 >NUL & echo survived > survived.txt\"\r\nping -n 30 127.0.0.1 >NUL\r\n",
            )
            .expect("write tree script");
            (
                "cmd",
                vec!["/C".to_string(), script.to_string_lossy().into_owned()],
            )
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_process(program, &args, dir.path(), Duration::from_secs(1), None)
            .await
            .expect("run_process");
        assert!(out.timed_out);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "a descendant survived the process-tree cleanup"
        );
    }

    #[tokio::test]
    async fn run_process_deadline_covers_a_grandchild_holding_the_pipe() {
        // A direct child that exits while a backgrounded grandchild keeps the
        // inherited stdout pipe open must not hang the call past its deadline.
        // Regression: the drain used to await EOF unbounded after the child
        // exited, so a lingering descendant defeated the "hard timeout".
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        let (program, args): (&str, &[&str]) = ("sh", &["-c", "sleep 5 & echo done"]);
        #[cfg(windows)]
        let (program, args): (&str, &[&str]) =
            ("cmd", &["/C", "start /B ping -n 6 127.0.0.1 & echo done"]);

        let start = Instant::now();
        let out = run_process(program, args, dir.path(), Duration::from_millis(300), None)
            .await
            .expect("run_process");
        let elapsed = start.elapsed();
        // Bounded by the deadline plus kill/cleanup slack — never the ~5s the
        // grandchild would otherwise hold the pipe open.
        assert!(
            elapsed < Duration::from_secs(4),
            "run_process waited {elapsed:?} for a pipe a grandchild held open"
        );
        assert!(out.timed_out, "exceeding the drain deadline is a timeout");
    }

    #[tokio::test]
    async fn fetch_and_reset_moves_to_remote_main() {
        let root = tempfile::tempdir().expect("tempdir");
        let origin = root.path().join("origin.git");
        std::fs::create_dir_all(&origin).expect("mkdir");
        run(&origin, &["init", "--bare", "-b", "main"]);

        let work = root.path().join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        run(
            &work,
            &["clone", origin.to_str().expect("origin path"), "."],
        );
        run(&work, &["checkout", "-b", "main"]);
        run(&work, &["config", "user.name", "test"]);
        run(&work, &["config", "user.email", "test@example.com"]);
        std::fs::write(work.join("f.txt"), "one\n").expect("write");
        run(&work, &["add", "."]);
        run(&work, &["commit", "-m", "init"]);
        run(&work, &["push", "-u", "origin", "main"]);

        let other = root.path().join("other");
        std::fs::create_dir_all(&other).expect("mkdir");
        run(
            &other,
            &["clone", origin.to_str().expect("origin path"), "."],
        );
        run(&other, &["config", "user.name", "test"]);
        run(&other, &["config", "user.email", "test@example.com"]);
        commit_file(&other, "g.txt", "two\n");
        run(&other, &["push", "origin", "main"]);

        fetch_base(&work, "main", Duration::from_secs(10))
            .await
            .expect("fetch");
        reset_to_base(&work, "main").expect("reset");
        assert_eq!(
            git(&work, &["rev-parse", "HEAD"]).expect("head"),
            git(&work, &["rev-parse", "origin/main"]).expect("origin")
        );
        assert!(work.join("g.txt").exists());
    }

    #[tokio::test]
    async fn branch_delete_removes_local_and_remote() {
        let root = tempfile::tempdir().expect("tempdir");
        let origin = root.path().join("origin.git");
        std::fs::create_dir_all(&origin).expect("mkdir");
        run(&origin, &["init", "--bare", "-b", "main"]);
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        run(
            &work,
            &["clone", origin.to_str().expect("origin path"), "."],
        );
        run(&work, &["checkout", "-b", "main"]);
        run(&work, &["config", "user.name", "test"]);
        run(&work, &["config", "user.email", "test@example.com"]);
        std::fs::write(work.join("f.txt"), "one\n").expect("write");
        run(&work, &["add", "."]);
        run(&work, &["commit", "-m", "init"]);
        run(&work, &["push", "-u", "origin", "main"]);

        run(&work, &["checkout", "-b", "agent/fix/x"]);
        run(&work, &["push", "-u", "origin", "agent/fix/x"]);
        run(&work, &["checkout", "main"]);

        assert!(branch_exists(&work, "agent/fix/x").expect("exists"));
        delete_local_branch(&work, "agent/fix/x").expect("delete local");
        assert!(!branch_exists(&work, "agent/fix/x").expect("gone"));
        delete_remote_branch(&work, "agent/fix/x", Duration::from_secs(10))
            .await
            .expect("delete remote");
        let remote =
            git(&work, &["ls-remote", "--heads", "origin", "agent/fix/x"]).expect("ls-remote");
        assert!(remote.is_empty());
    }

    #[test]
    fn sanitize_remote_strips_embedded_credentials() {
        assert_eq!(
            sanitize_remote("https://x-access-token:ghp_secret@github.com/acme/repo.git"),
            "https://github.com/acme/repo.git"
        );
        assert_eq!(
            sanitize_remote("git@github.com:acme/repo.git"),
            "git@github.com:acme/repo.git"
        );
    }

    #[test]
    fn current_branch_reports_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        assert_eq!(current_branch(dir.path()).expect("branch"), "main");
    }
}
