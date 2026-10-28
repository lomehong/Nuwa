//! Actual vendored AgentTool::execute calls using the production OS wrapper.
//! Every sensitive file below is synthetic and confined to a temporary checkout.

use super::*;
use crate::guard::Guards;
use rpi_agent::error::AgentError;
use rpi_agent::types::{AgentToolResult, TextContentOrImage};
use rpi_agent::AgentTool;
use rpi_tools::{
    create_edit_tool, create_find_tool, create_grep_tool, create_ls_tool, create_read_tool,
    create_write_tool, ExecutionToolContext,
};
use serde_json::{json, Value};

const SENTINEL: &str = "BATCH4_SYNTHETIC_DENIED_SENTINEL";
const SENSITIVE: &[&str] = &[
    ".env",
    "nested/.env.production",
    "keys/deploy.pem",
    ".git/config",
    ".ssh/id_ed25519",
    ".aws/credentials",
    ".nuwa/private/oracle.txt",
    "instructions-deadbeef.json",
    "sessions/raw.jsonl",
    "owner-vault/oracle.txt",
    "nested/owner-private.bin",
    "owner’s.txt",
    "runtime-state/raw.jsonl",
];

struct Fixture {
    _temp: tempfile::TempDir,
    env: Arc<LocalExecutionEnv>,
    ctx: ExecutionToolContext,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("synthetic checkout");
        let mut guards = Guards::for_checkout(
            temp.path(),
            vec!["oracle.py".into(), "protected/".into()],
            vec![
                "owner-vault/".into(),
                "nested/owner-private.*".into(),
                "owner’s.txt".into(),
            ],
        )
        .expect("rooted guards");
        let root = guards.policy().expect("policy").root().to_path_buf();
        guards
            .deny_private_path(&root.join("runtime-state"))
            .expect("private path");
        for path in SENSITIVE {
            seed(&root, path, SENTINEL);
        }
        seed(
            &root,
            "src/main.rs",
            "first line\nvisible needle\nlast line\n",
        );
        seed(&root, "oracle.py", "immutable assertion\n");
        seed(&root, "protected/assertion.txt", "immutable helper\n");
        seed(&root, "nuwa.repo.json", "{}\n");
        seed(&root, "Résumé note.rs", "unicode source\n");
        let inner = Arc::new(OsExecutionEnv::with_cwd(root));
        let env = Arc::new(LocalExecutionEnv::new(
            inner,
            guards.policy().expect("policy"),
        ));
        let env_dyn: Arc<dyn ExecutionEnv> = env.clone();
        let mut_env: Arc<dyn MutatingEnv> = env.clone();
        let ctx = ExecutionToolContext::new(env_dyn, Some(mut_env));
        Self {
            _temp: temp,
            env,
            ctx,
        }
    }

    fn assert_no_sensitive_delegation(&self) {
        let delegated = self.env.delegated_reads.lock().expect("read log");
        for relative in SENSITIVE {
            let sensitive = self.env.cwd().join(relative);
            assert!(
                !delegated.contains(&sensitive),
                "denied bytes were delegated: {relative}"
            );
        }
    }
}

fn seed(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed directories");
    std::fs::write(path, content).expect("seed synthetic bytes");
}

async fn execute(tool: Arc<dyn AgentTool>, args: Value) -> Result<String, AgentError> {
    let result: AgentToolResult = tool
        .execute(
            "batch4-test",
            args,
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await?;
    Ok(result
        .content
        .iter()
        .filter_map(|part| match part {
            TextContentOrImage::Text(t) => Some(t.text.as_str()),
            TextContentOrImage::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn denied_result<T: std::fmt::Debug>(result: Result<T, FileError>) {
    assert_eq!(
        result.expect_err("must deny before delegation").code,
        FileErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn all_six_real_tools_use_rooted_context_and_hide_recursive_sensitive_children() {
    let fixture = Fixture::new();
    let ctx = &fixture.ctx;
    let read = execute(
        create_read_tool(ctx, None),
        json!({"path":"src/main.rs", "offset":2, "limit":1}),
    )
    .await
    .expect("ranged tool read");
    assert!(read.contains("visible needle"));
    execute(
        create_edit_tool(ctx),
        json!({
            "path":"src/main.rs", "edits":[{"oldText":"needle", "newText":"repaired"}]
        }),
    )
    .await
    .expect("real edit");
    execute(
        create_write_tool(ctx),
        json!({"path":"new/deep/output.rs", "content":"visible new source\n"}),
    )
    .await
    .expect("new parents");
    let grep = execute(
        create_grep_tool(ctx, None),
        json!({"path":".", "pattern":"visible|BATCH4_SYNTHETIC"}),
    )
    .await
    .expect("root grep");
    assert!(grep.contains("src/main.rs"));
    assert!(!grep.contains(SENTINEL));
    let find = execute(
        create_find_tool(ctx, None),
        json!({"path":".", "pattern":"**"}),
    )
    .await
    .expect("wildcard root find");
    assert!(find.contains("src/main.rs"));
    let ls = execute(create_ls_tool(ctx, None), json!({"path":"."}))
        .await
        .expect("root ls");
    assert!(ls.contains("oracle.py"));
    for output in [&grep, &find, &ls] {
        for hidden in [
            ".env",
            "deploy.pem",
            ".git",
            ".ssh",
            ".aws",
            "owner-vault",
            "owner-private",
            "owner’s.txt",
            "runtime-state",
            "sessions",
            "instructions-deadbeef",
        ] {
            assert!(
                !output.contains(hidden),
                "denied name reached a tool: {hidden}"
            );
        }
    }
    assert!(
        fixture
            .env
            .delegated_read_count
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0,
        "positive control did not delegate"
    );
    fixture.assert_no_sensitive_delegation();
    assert!(
        std::fs::read_to_string(fixture.env.cwd().join("src/main.rs"))
            .expect("edited")
            .contains("repaired")
    );
    assert!(fixture.env.cwd().join("new/deep/output.rs").is_file());
}

#[tokio::test]
async fn explicit_sensitive_roots_fail_for_every_tool_without_reading_bytes() {
    let fixture = Fixture::new();
    for path in SENSITIVE
        .iter()
        .copied()
        .chain([".git", "owner-vault", "runtime-state"])
    {
        for (tool, args) in [
            (
                create_read_tool(&fixture.ctx, None),
                json!({"path":path, "offset":2, "limit":1}),
            ),
            (
                create_edit_tool(&fixture.ctx),
                json!({"path":path, "edits":[{"oldText":SENTINEL, "newText":"changed"}]}),
            ),
            (
                create_write_tool(&fixture.ctx),
                json!({"path":path, "content":"changed"}),
            ),
            (
                create_grep_tool(&fixture.ctx, None),
                json!({"path":path, "pattern":"."}),
            ),
            (
                create_find_tool(&fixture.ctx, None),
                json!({"path":path, "pattern":"**"}),
            ),
            (create_ls_tool(&fixture.ctx, None), json!({"path":path})),
        ] {
            let error = execute(tool, args).await.expect_err("explicit denied root");
            assert!(error.to_string().contains("permission_denied"));
        }
    }
    assert!(fixture
        .env
        .delegated_reads
        .lock()
        .expect("reads")
        .is_empty());
    assert_eq!(
        fixture
            .env
            .delegated_read_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    fixture.assert_no_sensitive_delegation();
}

#[tokio::test]
async fn direct_ranged_metadata_and_all_mutation_endpoints_authorize_before_delegating() {
    let fixture = Fixture::new();
    let env = &fixture.env;
    for path in SENSITIVE {
        denied_result(env.read_text_file(path, None).await);
        denied_result(env.read_text_lines(path, Some(1), None).await);
        denied_result(env.read_binary_file(path, None).await);
        denied_result(env.read_binary_file_range(path, 1, 4, None).await);
        denied_result(env.file_info(path, None).await);
        denied_result(env.exists(path, None).await);
        denied_result(env.canonical_path(path, None).await);
        denied_result(env.list_dir(path, None).await);
        denied_result(
            env.append_file(path, FileContent::from("changed".to_string()), None)
                .await,
        );
        denied_result(
            env.write_file(path, FileContent::from("changed".to_string()), None)
                .await,
        );
        denied_result(env.rename_file(path, "copied.rs", None).await);
        denied_result(env.rename_file("src/main.rs", path, None).await);
        denied_result(env.create_dir(path, true, None).await);
        denied_result(env.remove(path, true, true, None).await);
    }
    // An ordinary copy composed of read + write cannot read a denied source or
    // overwrite a protected destination. FileSystem has no separate copy API.
    let ordinary = env
        .read_binary_file("src/main.rs", None)
        .await
        .expect("copy source");
    denied_result(
        env.write_file("nuwa.repo.json", FileContent::Bytes(ordinary), None)
            .await,
    );
    denied_result(env.create_temp_dir(None, None).await);
    denied_result(env.create_temp_file("outside", ".tmp", None).await);
    fixture.assert_no_sensitive_delegation();
}

#[tokio::test]
async fn forbidden_oracle_and_configuration_remain_readable_but_immutable() {
    let fixture = Fixture::new();
    for path in ["oracle.py", "nuwa.repo.json", "protected/assertion.txt"] {
        execute(create_read_tool(&fixture.ctx, None), json!({"path":path}))
            .await
            .expect("safe protected read");
        execute(
            create_write_tool(&fixture.ctx),
            json!({"path":path, "content":"tampered"}),
        )
        .await
        .expect_err("protected write");
        execute(create_edit_tool(&fixture.ctx), json!({
            "path":path, "edits":[{"oldText":if path == "nuwa.repo.json" { "{}" } else if path == "oracle.py" { "immutable assertion" } else { "immutable helper" }, "newText":"tampered"}]
        })).await.expect_err("protected edit");
    }
    denied_result(fixture.env.rename_file("protected", "moved", None).await);
    denied_result(fixture.env.remove("protected", true, false, None).await);
    assert_eq!(
        std::fs::read_to_string(fixture.env.cwd().join("oracle.py")).expect("oracle"),
        "immutable assertion\n"
    );
    // Move/remove a writable parent containing a specifically protected file.
    seed(fixture.env.cwd(), "bundle/oracle.py", "nested assertion");
    denied_result(fixture.env.rename_file("bundle", "moved", None).await);
    denied_result(fixture.env.remove("bundle", true, false, None).await);
    fixture.assert_no_sensitive_delegation();
}

#[tokio::test]
async fn normalization_fuzzy_lookup_and_path_aliases_cannot_escape_policy() {
    let fixture = Fixture::new();
    let read = create_read_tool(&fixture.ctx, None);
    assert!(
        execute(read.clone(), json!({"path":"@Résumé\u{00a0}note.rs"}))
            .await
            .expect("Unicode spaces and @")
            .contains("unicode source")
    );
    // The literal ASCII apostrophe does not exist; the read tool finds a smart
    // quote variant. That actual variant must be denied before its first read.
    execute(read.clone(), json!({"path":"owner's.txt"}))
        .await
        .expect_err("fuzzy denied variant");
    for path in [
        "../outside",
        "src/../../outside",
        "@../outside",
        "~/.env",
        "file:///tmp/outside",
        r"C:relative",
        r"\rooted",
        r"\\server\share\file",
        r"\\?\C:\file",
        r"\\.\GLOBALROOT\file",
        "src/main.rs:stream",
        ".env.",
        ".env ",
        "dir./file",
        "CON",
        "NUL.txt",
    ] {
        execute(read.clone(), json!({"path":path}))
            .await
            .expect_err("unsafe alias read");
        execute(
            create_write_tool(&fixture.ctx),
            json!({"path":path, "content":"escape"}),
        )
        .await
        .expect_err("unsafe alias write");
    }
    let sibling = fixture.env.cwd().with_file_name(format!(
        "{}-sibling",
        fixture
            .env
            .cwd()
            .file_name()
            .expect("name")
            .to_string_lossy()
    ));
    execute(
        read,
        json!({"path":sibling.join("outside").to_string_lossy()}),
    )
    .await
    .expect_err("sibling prefix");
    denied_result(fixture.env.join_path(&["src", "../outside"], None).await);
    fixture.assert_no_sensitive_delegation();
}

#[tokio::test]
async fn missing_write_parents_and_directory_rename_destination_are_checked() {
    let fixture = Fixture::new();
    execute(
        create_write_tool(&fixture.ctx),
        json!({"path":"fresh/deeper/file.rs", "content":"source"}),
    )
    .await
    .expect("safe missing parents");
    for path in [
        "missing/.env/child.rs",
        "missing/new.pem",
        "owner-vault/new/deep.rs",
        "protected/new/deep.rs",
    ] {
        execute(
            create_write_tool(&fixture.ctx),
            json!({"path":path, "content":"denied"}),
        )
        .await
        .expect_err("unsafe parents");
    }
    assert!(
        !fixture.env.cwd().join("missing").exists(),
        "denied writes created parents"
    );
    seed(fixture.env.cwd(), "movable/assertion.txt", "source");
    denied_result(fixture.env.rename_file("movable", "protected", None).await);
    denied_result(
        fixture
            .env
            .rename_file("movable", "owner-vault", None)
            .await,
    );
    fixture
        .env
        .rename_file("fresh", "renamed", None)
        .await
        .expect("safe directory rename");
    assert!(fixture.env.cwd().join("renamed/deeper/file.rs").is_file());
    assert!(
        std::ptr::eq(
            fixture.env.mutation_registry(),
            fixture.env.inner.mutation_registry()
        ),
        "wrapper replaced OS mutation registry"
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        fixture
            .env
            .read_binary_file("src/main.rs", Some(&cancel))
            .await
            .expect_err("cancel")
            .code,
        FileErrorCode::Aborted
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_symlink_escape_and_replaced_missing_parent_never_delegate_reads() {
    let fixture = Fixture::new();
    fixture
        .env
        .absolute_path("escape/missing/child.rs", None)
        .await
        .expect("initial missing parent");
    let outside = tempfile::tempdir().expect("outside synthetic tree");
    seed(outside.path(), "sentinel.txt", SENTINEL);
    std::os::unix::fs::symlink(outside.path(), fixture.env.cwd().join("escape")).expect("symlink");
    std::os::unix::fs::symlink(
        fixture.env.cwd().join(".env"),
        fixture.env.cwd().join("alias.txt"),
    )
    .expect("in-root link");
    link_adversaries(&fixture).await;
}

#[cfg(windows)]
#[tokio::test]
async fn windows_junction_reparse_escape_never_delegates_reads() {
    let fixture = Fixture::new();
    fixture
        .env
        .absolute_path("escape/missing/child.rs", None)
        .await
        .expect("initial missing parent");
    let outside = tempfile::tempdir().expect("outside synthetic tree");
    seed(outside.path(), "sentinel.txt", SENTINEL);
    let link = fixture.env.cwd().join("escape");
    // Directory junctions normally need no symlink privilege. Only synthetic
    // temp paths go to cmd; no environment/config/credential output is read.
    let output = std::process::Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(outside.path())
        .output();
    if !output.is_ok_and(|o| o.status.success()) {
        eprintln!(
            "SKIP: Windows could not create a temporary junction; reparse test not exercised"
        );
        return;
    }
    link_adversaries(&fixture).await;
    // Remove only the junction, never the target tree; avoid recursive tempdir
    // cleanup implementations walking a junction on older Windows versions.
    std::fs::remove_dir(link).expect("remove synthetic junction");
}

#[cfg(any(unix, windows))]
async fn link_adversaries(fixture: &Fixture) {
    execute(
        create_read_tool(&fixture.ctx, None),
        json!({"path":"escape/sentinel.txt"}),
    )
    .await
    .expect_err("link read");
    execute(
        create_write_tool(&fixture.ctx),
        json!({"path":"escape/missing/child.rs", "content":"escape"}),
    )
    .await
    .expect_err("link write");
    for (tool, args) in [
        (
            create_grep_tool(&fixture.ctx, None),
            json!({"path":".", "pattern":"BATCH4_SYNTHETIC"}),
        ),
        (
            create_find_tool(&fixture.ctx, None),
            json!({"path":".", "pattern":"**"}),
        ),
        (create_ls_tool(&fixture.ctx, None), json!({"path":"."})),
    ] {
        let output = execute(tool, args).await.expect("root link filtering");
        assert!(!output.contains("escape"));
        assert!(!output.contains("alias.txt"));
        assert!(!output.contains(SENTINEL));
    }
    denied_result(
        fixture
            .env
            .read_binary_file_range("escape/sentinel.txt", 0, 4, None)
            .await,
    );
    assert!(fixture
        .env
        .delegated_reads
        .lock()
        .expect("reads")
        .iter()
        .all(|p| !p.starts_with(fixture.env.cwd().join("escape"))));
    fixture.assert_no_sensitive_delegation();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_case_folding_and_short_name_aliases_fail_closed() {
    let fixture = Fixture::new();
    for path in [
        ".ENV",
        "KEYS/DEPLOY.PEM",
        "OWNER-VAULT/oracle.txt",
        "NUWA.REPO.JSON",
        "src/MAIN.RS:stream",
        "src/MAIN.RS.",
        "SRC~1/main.rs",
    ] {
        execute(
            create_write_tool(&fixture.ctx),
            json!({"path":path, "content":"tampered"}),
        )
        .await
        .expect_err("Windows write alias");
    }
    for path in [".ENV", "KEYS/DEPLOY.PEM", "OWNER-VAULT/oracle.txt"] {
        execute(create_read_tool(&fixture.ctx, None), json!({"path":path}))
            .await
            .expect_err("Windows read alias");
    }
    fixture.assert_no_sensitive_delegation();
}

#[test]
fn legacy_guards_are_rooted_and_existing_policy_cannot_move_to_another_checkout() {
    let first = tempfile::tempdir().expect("first");
    let second = tempfile::tempdir().expect("second");
    let guards = Guards::new(vec!["oracle.py".into()])
        .rooted_for(first.path())
        .expect("upgrade legacy");
    assert!(guards.check("read", &json!({"path":".env"})).is_err());
    assert!(guards.check("read", &json!({"path":"oracle.py"})).is_ok());
    assert!(guards.check("write", &json!({"path":"oracle.py"})).is_err());
    assert!(guards.rooted_for(second.path()).is_err());
    assert!(Guards::for_checkout(first.path(), vec!["[".into()], Vec::new()).is_err());
}
