//! Nuwa 本地补丁(2026-10-21): os_env 大输出管道阻塞回归测试。
//!
//! 旧实现先等 `child.wait()` 再 join stdout/stderr 读流：子进程输出超过管道
//! 缓冲(~64KB)时 write 阻塞、子进程不退出、读流永不推进，活锁到超时。本测试
//! 用真实 `OsExecutionEnv` + 真实 shell 输出 256KiB（远超 64KB 管道容量），
//! 断言在超时预算内正常完成并拿到完整 stdout。内存假环境测不到该缺陷（没有
//! 真实 OS 管道），必须在真机 shell 上跑；无 bash 的平台按既有约定跳过。

use rpi_tools::{ExecutionErrorCode, OsExecutionEnv, Shell, ShellExecOptions};

/// 256KiB 的 'a'：`head -c` 读 /dev/zero（git-bash / POSIX 均可用），`tr` 换成
/// 可打印字符。输出远超管道缓冲，旧实现必然活锁到超时。
const LARGE_STDOUT_CMD: &str = "head -c 262144 /dev/zero | tr '\\0' 'a'";
const LARGE_STDOUT_LEN: usize = 262144;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn os_large_stdout_does_not_deadlock_until_timeout() {
    let env = OsExecutionEnv::new();
    // 超时给足 30s：修复后 256KiB 应在毫秒级完成；若仍活锁，测试会在 30s
    // 处以 Timeout 错误失败而不是挂死。
    let result = env
        .exec(
            LARGE_STDOUT_CMD,
            ShellExecOptions {
                timeout: Some(30.0),
                ..Default::default()
            },
        )
        .await;

    let output = match result {
        // 无 bash 的平台（约定内的环境缺失）跳过。
        Err(error) if error.code == ExecutionErrorCode::ShellUnavailable => return,
        other => other.expect("large stdout must complete without timing out"),
    };

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        output.stdout.len(),
        LARGE_STDOUT_LEN,
        "stdout must be captured in full, not truncated by the pipe deadlock"
    );
    assert!(
        output.stdout.bytes().all(|b| b == b'a'),
        "stdout content corrupted"
    );
    assert!(output.stderr.is_empty(), "unexpected stderr: {}", output.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn os_large_stderr_does_not_deadlock_until_timeout() {
    let env = OsExecutionEnv::new();
    // 同样的活锁形状发生在 stderr 管道上：把大输出重定向到 stderr。
    let result = env
        .exec(
            &format!("({LARGE_STDOUT_CMD}) >&2"),
            ShellExecOptions {
                timeout: Some(30.0),
                ..Default::default()
            },
        )
        .await;

    let output = match result {
        Err(error) if error.code == ExecutionErrorCode::ShellUnavailable => return,
        other => other.expect("large stderr must complete without timing out"),
    };

    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stderr.len(), LARGE_STDOUT_LEN);
    assert!(output.stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn os_small_output_still_completes() {
    //  sanity：小输出路径（16 字节，旧实现也正常）不被并发驱动改造破坏。
    let env = OsExecutionEnv::new();
    let result = env
        .exec(
            "printf 'hello'",
            ShellExecOptions {
                timeout: Some(15.0),
                ..Default::default()
            },
        )
        .await;
    let output = match result {
        Err(error) if error.code == ExecutionErrorCode::ShellUnavailable => return,
        other => other.expect("small output must complete"),
    };
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "hello");
}
