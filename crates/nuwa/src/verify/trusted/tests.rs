use super::*;
use crate::verify;

const ORACLE: &str = r#"import os, pathlib, runpy, sys
import helper
try:
    root = pathlib.Path(os.environ['NUWA_VERIFY_ROOT'])
    if root.name != 'target' or (root / '.git').exists():
        raise RuntimeError('invalid verification root')
    value = runpy.run_path(str(root / 'app.py'))['calculate']()
    expected = helper.expected()
except BaseException:
    sys.exit(2)
print('PRIVATE_ORACLE_SENTINEL')
sys.exit(0 if value == expected else 1)
"#;
const HELPER: &str = "import pathlib\ndef expected():\n    return int(pathlib.Path(__file__).with_name('expected.txt').read_text())\n";
const BUG: &str = "def calculate():\n    return 1\n";
const FIX: &str = "def calculate():\n    return 2\n";

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    base: String,
    owner: TrustedInstructions,
}

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("synthetic git fixture")
        .status;
    assert!(status.success(), "synthetic git operation failed");
}

fn command() -> &'static str {
    if cfg!(windows) {
        "python checks/oracle.py"
    } else {
        "python3 checks/oracle.py"
    }
}

impl Fixture {
    fn new(script: &str, launcher: &str, timeout: u64, tracked: bool) -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("checks")).expect("fixture directories");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "synthetic-owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("app.py"), BUG).expect("bug");
        std::fs::write(repo.join("checks/oracle.py"), script).expect("oracle");
        #[cfg(unix)]
        if launcher == "checks/oracle.py" {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                repo.join("checks/oracle.py"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("executable owner entrypoint");
        }
        std::fs::write(repo.join("checks/helper.py"), HELPER).expect("helper");
        std::fs::write(repo.join("checks/expected.txt"), "2\n").expect("fixture");
        let config = serde_json::json!({
            "repo": "synthetic", "failing_check": { "command": "echo ok" },
            "verification_policy": "strict",
            "trusted_verification": {
                "check": { "command": launcher, "timeout_secs": timeout },
                "protected_inputs": ["checks/oracle.py", "checks/helper.py", "checks/expected.txt"]
            }
        });
        let bytes = serde_json::to_vec(&config).expect("config JSON");
        if tracked {
            std::fs::write(repo.join("nuwa.repo.json"), &bytes).expect("config");
            git(&repo, &["add", "."]);
        } else {
            // The owner-seeded convention also supports non-Git oracle assets.
            git(&repo, &["add", "app.py"]);
        }
        git(&repo, &["commit", "-m", "synthetic pinned base"]);
        if !tracked {
            std::fs::write(repo.join("nuwa.repo.json"), &bytes).expect("seed config");
        }
        let base = git_ops::git(&repo, &["rev-parse", "HEAD"]).expect("base SHA");
        let owner = TrustedInstructions::load(
            &repo,
            &repo.join("nuwa.repo.json"),
            &base,
            &root.path().join("owner-private"),
        )
        .expect("authoritative owner");
        Self {
            _root: root,
            repo,
            base,
            owner,
        }
    }

    async fn prepare(&self) -> anyhow::Result<VerificationContext> {
        prepare(
            &self.repo,
            &self.base,
            Some(&self.owner),
            &Secrets::default(),
        )
        .await
    }

    fn workspace(&self) -> PathBuf {
        let workspace = self._root.path().join("workspace");
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().expect("path"),
                &self.base,
            ],
        );
        workspace
    }

    async fn prepare_pool(&self, workspace: &Path) -> anyhow::Result<VerificationContext> {
        prepare_workspace(
            workspace,
            &self.repo,
            &self.base,
            Some(&self.owner),
            &Secrets::default(),
        )
        .await
    }

    fn fix(&self) {
        std::fs::write(self.repo.join("app.py"), FIX).expect("candidate fix");
    }

    fn adopt_config(&mut self, config: serde_json::Value) {
        std::fs::write(
            self.repo.join("nuwa.repo.json"),
            serde_json::to_vec(&config).expect("JSON"),
        )
        .expect("owner configuration update");
        git(&self.repo, &["add", "nuwa.repo.json"]);
        git(&self.repo, &["commit", "-m", "owner configuration"]);
        self.base = git_ops::git(&self.repo, &["rev-parse", "HEAD"]).expect("new base");
        self.owner = TrustedInstructions::load(
            &self.repo,
            &self.repo.join("nuwa.repo.json"),
            &self.base,
            &self._root.path().join("owner-private"),
        )
        .expect("new owner authority");
    }

    async fn verify(&self, context: &VerificationContext) -> anyhow::Result<VerificationEvidence> {
        verify::verify(
            &self.repo,
            &issue(),
            &self.base,
            &Secrets::default(),
            context,
        )
        .await
    }
}

fn issue() -> Issue {
    serde_json::from_value(serde_json::json!({
        "fingerprint": "synthetic-oracle", "title": "synthetic", "language": "python",
        "failing_check": { "command": "echo ok", "timeout_secs": 10 },
        // Even agent/issue reproducer claims cannot replace an owner oracle.
        "reproduce_check": { "command": "echo agent-claimed-success" }
    }))
    .expect("synthetic issue")
}

#[test]
fn completed_exit_codes_are_red_or_green_even_when_cargo_uses_101() {
    for code in [0, 1, 2, 101] {
        let out = verify::CheckOutput {
            ok: code == 0,
            exit_code: Some(code),
            timed_out: false,
            output: String::new(),
        };
        require_valid_check(&out, "base failing_check").expect("completed exit is valid");
    }

    let timed_out = verify::CheckOutput {
        ok: false,
        exit_code: None,
        timed_out: true,
        output: String::new(),
    };
    assert!(require_valid_check(&timed_out, "base failing_check").is_err());

    let no_exit = verify::CheckOutput {
        ok: false,
        exit_code: None,
        timed_out: false,
        output: String::new(),
    };
    assert!(require_valid_check(&no_exit, "base failing_check").is_err());
}

#[tokio::test]
async fn genuine_fix_is_owner_trusted_even_when_failing_check_base_is_green() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("owner-red before repair");
    assert!(context.is_trusted());
    assert!(context
        .assert_verified_candidate(&fixture.repo, &fixture.base)
        .is_err());
    assert_eq!(context.protected_paths().len(), 4);
    fixture.fix();
    let evidence = fixture.verify(&context).await.expect("owner-green fix");
    assert_eq!(evidence.mode, "owner_trusted");
    assert_eq!(evidence.provenance.trust_level, "owner_trusted");
    assert!(
        evidence.failing_base.ok,
        "base_green is NOT trusted reproduction"
    );
    assert_eq!(
        evidence
            .reproduce_base
            .as_ref()
            .expect("owner red")
            .exit_code,
        Some(1)
    );
    assert_eq!(
        evidence
            .reproduce_branch
            .as_ref()
            .expect("owner green")
            .exit_code,
        Some(0)
    );
    assert_eq!(
        evidence.provenance.candidate_tree,
        verify::candidate_tree(&fixture.repo).expect("tree")
    );
    assert!(evidence.provenance.manifest_sha256.is_some());
    assert!(!evidence
        .detail(&issue())
        .contains("PRIVATE_ORACLE_SENTINEL"));
    assert!(!format!("{context:?}").contains(HELPER));
    context
        .assert_verified_candidate(&fixture.repo, &fixture.base)
        .expect("delivery bound");
    std::fs::write(fixture.repo.join("unverified.txt"), "later write").expect("later mutation");
    assert!(context
        .assert_verified_candidate(&fixture.repo, &fixture.base)
        .is_err());
}

#[tokio::test]
async fn noop_repro_only_branch_cheat_and_exception_swallow_do_not_fix_the_subject() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot before agent");
    assert!(fixture.verify(&context).await.is_err(), "noop must fail");
    std::fs::create_dir(fixture.repo.join(".nuwa")).expect("agent directory");
    std::fs::write(fixture.repo.join(".nuwa/repro.sh"), "exit 0\n").expect("agent proof");
    std::fs::write(fixture.repo.join(".nuwa/repro.cmd"), "@exit /b 0\r\n").expect("agent proof");
    assert!(
        fixture.verify(&context).await.is_err(),
        "agent proof is not owner proof"
    );
    git(&fixture.repo, &["checkout", "-b", "agent/fix/branch-cheat"]);
    std::fs::write(fixture.repo.join("app.py"),
        "import pathlib\ndef calculate():\n    return 2 if pathlib.Path(__file__).with_name('.git').exists() else 1\n")
        .expect("metadata cheat");
    assert!(
        fixture.verify(&context).await.is_err(),
        "export has no branch/Git metadata"
    );
    std::fs::write(fixture.repo.join("app.py"),
        "def calculate():\n    try:\n        raise ValueError('synthetic')\n    except Exception:\n        return None\n")
        .expect("swallowed error");
    assert!(
        fixture.verify(&context).await.is_err(),
        "swallowing is not correct output"
    );
    assert!(context
        .assert_verified_candidate(&fixture.repo, &fixture.base)
        .is_err());
}

#[tokio::test]
async fn tamper_delete_type_and_staged_executable_mode_are_refused() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    fixture.fix();
    fixture
        .verify(&context)
        .await
        .expect("initial verified fix");
    let helper = fixture.repo.join("checks/helper.py");
    std::fs::write(&helper, "def expected(): return 1\n").expect("weaken helper");
    assert!(context.assert_integrity(&fixture.repo).is_err());
    assert!(fixture.verify(&context).await.is_err());
    std::fs::write(&helper, HELPER).expect("test restores fixture");
    std::fs::remove_file(&helper).expect("delete helper");
    assert!(context.assert_integrity(&fixture.repo).is_err());
    std::fs::create_dir(&helper).expect("replace with directory");
    assert!(context.assert_integrity(&fixture.repo).is_err());
    std::fs::remove_dir(&helper).expect("remove synthetic directory");
    std::fs::write(&helper, HELPER).expect("test restores fixture");
    git(
        &fixture.repo,
        &["update-index", "--chmod=+x", "checks/helper.py"],
    );
    assert!(
        context.assert_integrity(&fixture.repo).is_err(),
        "Git mode is protected on Windows too"
    );
    assert!(context
        .assert_verified_candidate(&fixture.repo, &fixture.base)
        .is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn native_executable_mode_and_link_replacement_are_refused() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    let path = fixture.repo.join("checks/helper.py");
    let original = std::fs::metadata(&path).expect("mode").permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(context.assert_integrity(&fixture.repo).is_err());
    std::fs::set_permissions(&path, original).expect("restore mode");
    let outside = fixture._root.path().join("outside-helper.py");
    std::fs::write(&outside, HELPER).expect("identical outside file");
    std::fs::remove_file(&path).expect("remove synthetic helper");
    symlink(outside, path).expect("link");
    assert!(
        context.assert_integrity(&fixture.repo).is_err(),
        "identical bytes do not bless links"
    );
}

#[tokio::test]
async fn always_pass_setup_error_timeout_missing_and_unsafe_launcher_never_fallback() {
    for script in ["import sys\nsys.exit(0)\n", "import sys\nsys.exit(2)\n"] {
        let fixture = Fixture::new(script, command(), 10, true);
        assert!(fixture.prepare().await.is_err());
    }
    let timeout = Fixture::new("import time\ntime.sleep(30)\n", command(), 1, true);
    let error = timeout.prepare().await.expect_err("baseline timeout");
    assert!(format!("{error:#}").contains("timed out"));
    let missing = Fixture::new(ORACLE, command(), 10, true);
    std::fs::remove_file(missing.repo.join("checks/expected.txt")).expect("missing owner input");
    assert!(missing.prepare().await.is_err());
    let stateful = Fixture::new(
        "import pathlib, sys\np=pathlib.Path.cwd().parent/'phase-marker'\np.write_text('red')\nsys.exit(1)\n",
        command(), 10, true);
    assert!(
        stateful.prepare().await.is_err(),
        "phase artifacts must not survive red/green exports"
    );
    for launcher in [
        "missing-tool checks/oracle.py",
        "python missing.py",
        "python -c 'exit(1)'",
        "python checks/oracle.py && echo fake",
        "python ../target/checks/oracle.py",
    ] {
        let fixture = Fixture::new(ORACLE, launcher, 10, true);
        assert!(
            fixture.prepare().await.is_err(),
            "unsafe/unavailable oracle cannot downgrade"
        );
    }
}

#[tokio::test]
async fn tracked_truth_refuses_preparation_after_first_agent_write() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    std::fs::write(
        fixture.repo.join("checks/helper.py"),
        "def expected(): return 1\n",
    )
    .expect("agent wrote before prepare");
    assert!(
        fixture.prepare().await.is_err(),
        "tracked base is authoritative"
    );
    assert!(
        prepare(&fixture.repo, &fixture.base, None, &Secrets::default())
            .await
            .is_err(),
        "omitting owner authority is not a compatibility bypass"
    );
}

#[tokio::test]
async fn owner_seeded_inputs_are_frozen_before_repair_too() {
    for ignored in [false, true] {
        let fixture = Fixture::new(ORACLE, command(), 10, false);
        if ignored {
            std::fs::write(fixture.repo.join(".git/info/exclude"), "/checks/\n")
                .expect("ignored private inputs");
        }
        assert_eq!(fixture.owner.source(), TrustSource::PrivateCopy);
        let context = fixture.prepare().await.expect("owner-seeded oracle red");
        assert_eq!(context.excluded_delivery_paths().len(), 4);
        fixture.fix();
        let mut issue = issue();
        issue.forbidden_paths = context.protected_paths();
        let evidence = verify::verify(
            &fixture.repo,
            &issue,
            &fixture.base,
            &Secrets::default(),
            &context,
        )
        .await
        .expect("seeded oracle green with pipeline forbidden paths");
        assert_eq!(evidence.provenance.owner_source, Some("owner_seeded"));
        assert_eq!(evidence.diff.files.len(), 1);
        assert_eq!(evidence.diff.files[0].path, "app.py");
        let tree = &evidence.provenance.candidate_tree;
        assert_eq!(
            git_ops::git(&fixture.repo, &["ls-tree", "-r", "--name-only", tree])
                .expect("delivery paths"),
            "app.py"
        );
        // Even pre-staging identical private bytes may not deliver them.
        git(&fixture.repo, &["add", "-f", "checks/expected.txt"]);
        context
            .assert_verified_candidate(&fixture.repo, &fixture.base)
            .expect("identical seeded staging");
        let add = verify::delivery_add_args(&fixture.repo, &context.excluded_delivery_paths())
            .expect("staging arguments");
        git(
            &fixture.repo,
            &add.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let reset = verify::delivery_reset_args(&context.excluded_delivery_paths());
        git(
            &fixture.repo,
            &reset.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(
            git_ops::git(&fixture.repo, &["write-tree"]).expect("index"),
            *tree
        );
        std::fs::write(fixture.repo.join("checks/expected.txt"), "1\n")
            .expect("tamper seeded bytes");
        assert!(context.assert_integrity(&fixture.repo).is_err());
        assert!(fixture.verify(&context).await.is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.repo.join("checks/expected.txt")).expect("no restore"),
            "1\n"
        );
    }
}

#[tokio::test]
async fn private_config_never_excludes_tracked_oracle_inputs() {
    let mut fixture = Fixture::new(ORACLE, command(), 10, false);
    git(&fixture.repo, &["add", "checks"]);
    git(
        &fixture.repo,
        &["commit", "-m", "track oracle but not private config"],
    );
    fixture.base = git_ops::git(&fixture.repo, &["rev-parse", "HEAD"]).expect("base");
    let context = fixture
        .prepare()
        .await
        .expect("tracked oracle, private config");
    assert_eq!(context.excluded_delivery_paths(), ["nuwa.repo.json"]);
    fixture.fix();
    fixture.verify(&context).await.expect("genuine tracked fix");
    let path = fixture.repo.join("checks/expected.txt");
    std::fs::write(&path, "1\n").expect("tamper tracked input");
    assert!(fixture.verify(&context).await.is_err());
    assert_eq!(std::fs::read_to_string(path).expect("no restore"), "1\n");
}

#[tokio::test]
async fn staged_or_committed_owner_seed_is_not_silently_hidden() {
    let fixture = Fixture::new(ORACLE, command(), 10, false);
    git(&fixture.repo, &["add", "checks/expected.txt"]);
    assert!(fixture.prepare().await.is_err(), "not genuinely untracked");

    let fixture = Fixture::new(ORACLE, command(), 10, false);
    let context = fixture.prepare().await.expect("snapshot untracked inputs");
    fixture.fix();
    git(&fixture.repo, &["add", "checks/expected.txt"]);
    git(
        &fixture.repo,
        &["commit", "-m", "try to deliver private owner input"],
    );
    assert!(
        fixture.verify(&context).await.is_err(),
        "committed seed is visible"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pool_seeding_rejects_linked_source_and_destination_files_and_directories() {
    use std::os::unix::fs::symlink;
    for source in [true, false] {
        for directory in [true, false] {
            let fixture = Fixture::new(ORACLE, command(), 10, false);
            let workspace = fixture.workspace();
            let root = if source { &fixture.repo } else { &workspace };
            let outside = fixture._root.path().join("outside");
            std::fs::create_dir(&outside).expect("outside directory");
            std::fs::write(outside.join("helper.py"), HELPER).expect("outside sentinel");
            if source {
                if directory {
                    std::fs::rename(
                        root.join("checks"),
                        fixture._root.path().join("original-checks"),
                    )
                    .expect("move synthetic source checks");
                } else {
                    std::fs::remove_file(root.join("checks/helper.py"))
                        .expect("remove fixture file");
                }
            } else if !directory {
                std::fs::create_dir(root.join("checks")).expect("destination directory");
            }
            if directory {
                symlink(&outside, root.join("checks")).expect("directory link");
            } else {
                symlink(outside.join("helper.py"), root.join("checks/helper.py"))
                    .expect("file link");
            }
            assert!(fixture.prepare_pool(&workspace).await.is_err());
            assert_eq!(
                std::fs::read_to_string(outside.join("helper.py")).expect("no linked writes"),
                HELPER
            );
        }
    }
}

#[tokio::test]
async fn pool_seeding_refuses_existing_wrong_bytes_type_or_native_mode_without_restore() {
    for case in ["bytes", "type", "mode"] {
        let fixture = Fixture::new(ORACLE, command(), 10, false);
        let workspace = fixture.workspace();
        std::fs::create_dir(workspace.join("checks")).expect("destination checks");
        let destination = workspace.join("checks/helper.py");
        if case == "type" {
            std::fs::create_dir(&destination).expect("wrong input type");
        } else {
            std::fs::write(
                &destination,
                if case == "bytes" { "tampered" } else { HELPER },
            )
            .expect("existing destination");
        }
        let original_permissions = std::fs::metadata(&destination).expect("mode").permissions();
        if case == "mode" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
                    .expect("wrong native mode");
            }
            #[cfg(not(unix))]
            {
                let mut permissions = original_permissions.clone();
                permissions.set_readonly(true);
                std::fs::set_permissions(&destination, permissions).expect("wrong native mode");
            }
        }
        assert!(fixture.prepare_pool(&workspace).await.is_err(), "{case}");
        if case == "bytes" {
            assert_eq!(
                std::fs::read_to_string(&destination).expect("not restored"),
                "tampered"
            );
        } else if case == "type" {
            assert!(destination.is_dir());
        } else {
            assert_ne!(
                native_mode(&destination).expect("still wrong"),
                native_mode(&fixture.repo.join("checks/helper.py")).expect("source mode")
            );
            std::fs::set_permissions(&destination, original_permissions)
                .expect("fixture cleanup mode");
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn pool_seeding_refuses_windows_source_and_destination_junctions() {
    for source in [true, false] {
        let fixture = Fixture::new(ORACLE, command(), 10, false);
        let workspace = fixture.workspace();
        let root = if source { &fixture.repo } else { &workspace };
        let outside = fixture._root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside directory");
        std::fs::write(outside.join("helper.py"), HELPER).expect("outside sentinel");
        if source {
            std::fs::rename(
                root.join("checks"),
                fixture._root.path().join("original-checks"),
            )
            .expect("move synthetic source checks");
        }
        let junction = root.join("checks");
        let output = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .expect("synthetic junction setup");
        assert!(output.status.success(), "junction setup unavailable");
        assert!(fixture.prepare_pool(&workspace).await.is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("helper.py")).expect("sentinel"),
            HELPER
        );
        std::fs::remove_dir(junction).expect("remove only synthetic junction");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn missing_direct_entrypoint_interpreter_is_a_spawn_error_not_bug_evidence() {
    let fixture = Fixture::new(
        "#!/definitely/missing/nuwa-test-interpreter\n",
        "checks/oracle.py",
        10,
        true,
    );
    let error = fixture
        .prepare()
        .await
        .expect_err("missing executable interpreter");
    assert!(format!("{error:#}").contains("could not execute"));
}

#[tokio::test]
async fn staged_protected_bytes_cannot_hide_behind_identical_working_bytes() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    std::fs::write(
        fixture.repo.join("checks/helper.py"),
        "def expected(): return 1\n",
    )
    .expect("stage attack");
    git(&fixture.repo, &["add", "checks/helper.py"]);
    std::fs::write(fixture.repo.join("checks/helper.py"), HELPER).expect("conceal in worktree");
    assert!(context.assert_integrity(&fixture.repo).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn untracked_link_is_rejected_before_candidate_index_reads_escaping_bytes() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    fixture.fix();
    let outside = fixture._root.path().join("synthetic-outside.txt");
    std::fs::write(&outside, "synthetic sensitive sentinel\n").expect("sentinel");
    symlink(&outside, fixture.repo.join("escaping-link.txt")).expect("candidate link");
    assert!(fixture.verify(&context).await.is_err());
    assert_eq!(
        std::fs::read_to_string(outside).expect("sentinel intact"),
        "synthetic sensitive sentinel\n"
    );
}

#[tokio::test]
async fn original_oracle_survives_base_movement_and_new_green_baseline_is_refused() {
    for tracked in [true, false] {
        let fixture = Fixture::new(ORACLE, command(), 10, tracked);
        let context = fixture.prepare().await.expect("original baseline");
        std::fs::write(fixture.repo.join("upstream.txt"), "upstream\n").expect("upstream");
        git(&fixture.repo, &["add", "upstream.txt"]);
        git(&fixture.repo, &["commit", "-m", "unrelated upstream"]);
        let moved = git_ops::git(&fixture.repo, &["rev-parse", "HEAD"]).expect("moved base");
        fixture.fix();
        let evidence = verify::verify(
            &fixture.repo,
            &issue(),
            &moved,
            &Secrets::default(),
            &context,
        )
        .await
        .expect("same original oracle after merge");
        assert_eq!(evidence.provenance.base_sha, moved);
        assert_eq!(evidence.diff.files.len(), 1);
        assert_eq!(evidence.diff.files[0].path, "app.py");
        assert_eq!(context.base_sha(), fixture.base);
        assert!(context
            .assert_verified_candidate(&fixture.repo, &fixture.base)
            .is_err());
        git(&fixture.repo, &["add", "app.py"]);
        git(&fixture.repo, &["commit", "-m", "upstream already fixed"]);
        let green = git_ops::git(&fixture.repo, &["rev-parse", "HEAD"]).expect("green base");
        assert!(verify::verify(
            &fixture.repo,
            &issue(),
            &green,
            &Secrets::default(),
            &context
        )
        .await
        .is_err());
        assert!(
            context
                .assert_verified_candidate(&fixture.repo, &moved)
                .is_err(),
            "failed recheck revokes old proof"
        );
    }
}

#[tokio::test]
async fn moved_base_may_not_change_or_replace_the_original_owner_spec() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("original baseline");
    std::fs::write(fixture.repo.join("checks/expected.txt"), "3\n")
        .expect("upstream oracle change");
    git(&fixture.repo, &["commit", "-am", "change oracle"]);
    let changed = git_ops::git(&fixture.repo, &["rev-parse", "HEAD"]).expect("changed base");
    // Even restoring original candidate bytes must not adopt the changed base.
    std::fs::write(fixture.repo.join("checks/expected.txt"), "2\n")
        .expect("restore candidate fixture");
    fixture.fix();
    assert!(verify::verify(
        &fixture.repo,
        &issue(),
        &changed,
        &Secrets::default(),
        &context
    )
    .await
    .is_err());
}

#[tokio::test]
async fn strict_missing_oracle_is_refused_but_compatibility_is_explicitly_untrusted() {
    for policy in ["strict", "compatibility"] {
        let mut fixture = Fixture::new(ORACLE, command(), 10, true);
        fixture.adopt_config(serde_json::json!({
            "repo": "synthetic", "failing_check": {"command": "echo ok"},
            "verification_policy": policy
        }));
        let prepared = fixture.prepare().await;
        if policy == "strict" {
            assert!(
                prepared.is_err(),
                "must refuse before first agent/branch write"
            );
        } else {
            let context = prepared.expect("legacy compatibility");
            assert!(!context.is_trusted());
            std::fs::write(fixture.repo.join("fixed.txt"), "fixed\n").expect("legacy fix");
            let mut legacy_issue = issue();
            legacy_issue.reproduce_check = None;
            legacy_issue.failing_check.command = if cfg!(windows) {
                "if exist fixed.txt (exit 0) else (exit 1)".to_string()
            } else {
                "test -f fixed.txt".to_string()
            };
            let evidence = verify::verify(
                &fixture.repo,
                &legacy_issue,
                &fixture.base,
                &Secrets::default(),
                &context,
            )
            .await
            .expect("legacy verification");
            assert_eq!(evidence.provenance.trust_level, "legacy_untrusted");
            assert!(evidence.provenance.oracle_base_exit.is_none());
        }
    }
    let mut configured = Fixture::new("import sys\nsys.exit(0)\n", command(), 10, true);
    let mut config: serde_json::Value =
        serde_json::from_slice(&configured.owner.bytes).expect("JSON");
    config["verification_policy"] = serde_json::json!("compatibility");
    configured.adopt_config(config);
    assert!(
        configured.prepare().await.is_err(),
        "configured oracle NEVER falls back, even in compatibility"
    );
}

#[tokio::test]
async fn public_convenience_fields_cannot_downgrade_pinned_owner_policy() {
    let mut fixture = Fixture::new(ORACLE, command(), 10, true);
    fixture.owner.instructions.verification_policy = VerificationPolicy::Compatibility;
    fixture.owner.instructions.trusted_verification = None;
    let context = fixture.prepare().await.expect("reparsed pinned authority");
    assert!(context.is_trusted());
    assert!(
        fixture.verify(&context).await.is_err(),
        "issue green is not owner reproduction"
    );
}

#[tokio::test]
async fn private_snapshot_tampering_and_exported_check_tampering_are_detected() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    fixture.fix();
    let oracle = context.oracle.as_ref().expect("frozen oracle");
    std::fs::write(
        oracle.private.path().join("oracle/checks/helper.py"),
        "def expected(): return 2\n",
    )
    .expect("private snapshot attack");
    assert!(context.assert_integrity(&fixture.repo).is_err());
    assert!(fixture.verify(&context).await.is_err());

    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let context = fixture.prepare().await.expect("snapshot");
    std::fs::write(fixture.repo.join("app.py"),
        "import pathlib\ndef calculate():\n    pathlib.Path(__file__).with_name('checks').joinpath('helper.py').write_text('weakened')\n    return 2\n")
        .expect("target check tamper");
    assert!(
        fixture.verify(&context).await.is_err(),
        "protected export must also stay identical"
    );
}

#[test]
fn candidate_tree_includes_staged_unstaged_and_untracked_but_excludes_staged_owner_config() {
    let fixture = Fixture::new(ORACLE, command(), 10, true);
    let before =
        git_ops::git_raw(&fixture.repo, &["status", "--porcelain=v1", "-z"]).expect("status");
    let original = verify::candidate_tree(&fixture.repo).expect("original tree");
    assert_eq!(
        git_ops::git_raw(&fixture.repo, &["status", "--porcelain=v1", "-z"]).expect("status"),
        before
    );
    std::fs::write(
        fixture.repo.join("nuwa.repo.json"),
        "synthetic config tampering\n",
    )
    .expect("config");
    git(&fixture.repo, &["add", "nuwa.repo.json"]);
    assert_eq!(
        verify::candidate_tree(&fixture.repo).expect("excluded config tree"),
        original
    );
    fixture.fix();
    git(&fixture.repo, &["add", "app.py"]);
    let staged = verify::candidate_tree(&fixture.repo).expect("staged tree");
    assert_ne!(staged, original);
    std::fs::write(fixture.repo.join("app.py"), "def calculate(): return 3\n").expect("unstaged");
    let unstaged = verify::candidate_tree(&fixture.repo).expect("unstaged tree");
    assert_ne!(unstaged, staged);
    std::fs::write(fixture.repo.join("untracked.txt"), "delivered too\n").expect("untracked");
    assert_ne!(
        verify::candidate_tree(&fixture.repo).expect("untracked tree"),
        unstaged
    );
}

#[cfg(windows)]
#[test]
fn legacy_reproducer_copy_refuses_windows_junction_ancestors() {
    let root = tempfile::tempdir().expect("root");
    let repo = root.path().join("repo");
    let base = root.path().join("base");
    let outside = root.path().join("outside");
    for path in [&repo, &base, &outside] {
        std::fs::create_dir(path).expect("directory");
    }
    std::fs::create_dir(repo.join(".nuwa")).expect("safe source");
    std::fs::write(repo.join(".nuwa/repro.cmd"), "@exit /b 0\r\n").expect("safe script");
    std::fs::write(outside.join("repro.cmd"), "synthetic sentinel\r\n").expect("outside sentinel");
    let result = std::process::Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(base.join(".nuwa"))
        .arg(&outside)
        .output()
        .expect("synthetic junction setup");
    assert!(
        result.status.success(),
        "junction setup unavailable on this Windows host"
    );
    assert!(verify::copy_reproducer_if_present(&repo, &base).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("repro.cmd")).expect("sentinel"),
        "synthetic sentinel\r\n"
    );
    std::fs::remove_dir(base.join(".nuwa")).expect("remove synthetic junction only");
}

#[cfg(unix)]
#[test]
fn legacy_reproducer_copy_rejects_linked_source_and_destination_ancestors() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("root");
    let repo = root.path().join("repo");
    let base = root.path().join("base");
    let outside = root.path().join("outside");
    for path in [&repo, &base, &outside] {
        std::fs::create_dir(path).expect("directory");
    }
    std::fs::write(outside.join("repro.sh"), "synthetic sentinel\n").expect("outside sentinel");
    symlink(&outside, repo.join(".nuwa")).expect("source link");
    assert!(verify::find_reproducer(&repo).is_err());
    assert!(verify::copy_reproducer_if_present(&repo, &base).is_err());
    std::fs::remove_file(repo.join(".nuwa")).expect("remove test source link");
    std::fs::create_dir(repo.join(".nuwa")).expect("safe source directory");
    std::fs::write(repo.join(".nuwa/repro.sh"), "safe reproducer\n").expect("safe source");
    symlink(&outside, base.join(".nuwa")).expect("destination link");
    assert!(verify::copy_reproducer_if_present(&repo, &base).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("repro.sh")).expect("unchanged sentinel"),
        "synthetic sentinel\n"
    );
}
