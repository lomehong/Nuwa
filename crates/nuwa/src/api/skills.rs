//! `GET /api/v1/projects/{repo}/skills` — the project's repair-skill catalog.
//!
//! Skills are read-only in the console by design: built-ins ship with the
//! binary, repo skills live in the target repository's `.nuwa/skills/`. This
//! endpoint only reports what the runner would see, including parse warnings
//! and which definition is effective when a repo skill shadows a built-in.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::api::runners::effective_config;
use crate::api::ApiError;
use crate::serve::AppState;
use crate::skills::{self, Skill};
use shanhaijing::{RunnerRow, Store};

const MAX_BODY_CHARS: usize = 20_000;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ProjectSkillsResponse {
    pub repo: String,
    /// `pinned` when `nuwa.repo.json` lists skills, `auto` otherwise.
    pub selection_mode: String,
    pub allowlist: Vec<String>,
    /// Last repository path reported by a runner, when one is online/known.
    pub repo_path: Option<String>,
    pub skills: Vec<SkillDto>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SkillDto {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub languages: Vec<String>,
    pub matches: Vec<String>,
    pub body: String,
    pub body_truncated: bool,
    /// `builtin` or `repo`.
    pub source: String,
    pub origin: String,
    /// False for a built-in shadowed by a repo skill of the same name.
    pub effective: bool,
    pub overrides_builtin: bool,
    pub allowlisted: bool,
}

pub async fn project_skills(State(state): State<AppState>, Path(repo): Path<String>) -> Response {
    let repo = repo.trim().to_string();
    if repo.is_empty() || repo.chars().count() > 256 {
        return ApiError::validation(BTreeMap::from([(
            "repo".to_string(),
            "must be a non-empty repository identity".to_string(),
        )]))
        .into_response();
    }
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_response(&store, &repo)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("project skills: read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("project skills: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read project skills",
    )
    .into_response()
}

pub(crate) fn build_response(
    store: &Store,
    repo: &str,
) -> shanhaijing::ShanResult<ProjectSkillsResponse> {
    let runner = newest_runner(store, repo)?;
    let allowlist = runner
        .as_ref()
        .map(|runner| effective_config(&runner.config_json).skills)
        .unwrap_or_default();
    let repo_path = runner.as_ref().and_then(|runner| runner.repo_path.clone());

    let mut warnings = Vec::new();
    if runner.is_none() {
        warnings.push("没有该项目的运行器记录：仅显示内置技能，无法读取仓库技能".to_string());
    } else if repo_path.is_none() {
        warnings.push("运行器未上报 repo_path：无法读取仓库技能，仅显示内置技能".to_string());
    }

    let builtin = match skills::builtin() {
        Ok(skills) => skills,
        Err(error) => {
            warnings.push(format!("内置技能不可用：{error:#}"));
            Vec::new()
        }
    };
    let mut repo_skills = Vec::new();
    if let Some(path) = &repo_path {
        let (loaded, mut load_warnings) = skills::repo_skills(std::path::Path::new(path));
        warnings.append(&mut load_warnings);
        repo_skills = loaded;
    }

    // A duplicate repo name is ambiguous: keep the last (sorted) definition and
    // surface the conflict instead of silently picking one.
    let mut repo_by_name: BTreeMap<String, Skill> = BTreeMap::new();
    for skill in repo_skills {
        if let Some(previous) = repo_by_name.insert(skill.name.clone(), skill.clone()) {
            warnings.push(format!(
                "仓库技能 `{}` 重复：{} 覆盖 {}",
                skill.name, skill.origin, previous.origin
            ));
        }
    }
    let builtin_names: BTreeSet<String> = builtin.iter().map(|skill| skill.name.clone()).collect();

    let allowlisted = |name: &str| allowlist.iter().any(|entry| entry == name);
    let mut dtos = Vec::new();
    for skill in &builtin {
        dtos.push(dto(
            skill,
            "builtin",
            !repo_by_name.contains_key(&skill.name),
            false,
            allowlisted(&skill.name),
        ));
    }
    for skill in repo_by_name.values() {
        dtos.push(dto(
            skill,
            "repo",
            true,
            builtin_names.contains(&skill.name),
            allowlisted(&skill.name),
        ));
    }
    dtos.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| b.effective.cmp(&a.effective))
            .then_with(|| a.source.cmp(&b.source))
    });

    for name in &allowlist {
        if !dtos.iter().any(|skill| &skill.name == name) {
            warnings.push(format!("skills 白名单里的 `{name}` 不存在"));
        }
    }

    Ok(ProjectSkillsResponse {
        repo: repo.to_string(),
        selection_mode: if allowlist.is_empty() {
            "auto".to_string()
        } else {
            "pinned".to_string()
        },
        allowlist,
        repo_path,
        skills: dtos,
        warnings,
    })
}

fn newest_runner(store: &Store, repo: &str) -> shanhaijing::ShanResult<Option<RunnerRow>> {
    Ok(store
        .list_runners()?
        .into_iter()
        .filter(|runner| runner.repo == repo)
        .max_by_key(|runner| runner.heartbeat_at))
}

fn dto(
    skill: &Skill,
    source: &str,
    effective: bool,
    overrides_builtin: bool,
    allowlisted: bool,
) -> SkillDto {
    let truncated: String = skill.body.chars().take(MAX_BODY_CHARS).collect();
    let body_truncated = truncated.chars().count() < skill.body.chars().count();
    SkillDto {
        name: skill.name.clone(),
        version: skill.version,
        description: skill.description.clone(),
        languages: skill.languages.clone(),
        matches: skill.matches.clone(),
        body: truncated,
        body_truncated,
        source: source.to_string(),
        origin: skill.origin.clone(),
        effective,
        overrides_builtin,
        allowlisted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shanhaijing::RunnerUpsert;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        (dir, store)
    }

    fn seed_runner(store: &Store, repo: &str, repo_path: &std::path::Path, skills: &[&str]) {
        store
            .upsert_runner(&RunnerUpsert {
                id: "runner-1".into(),
                repo: repo.into(),
                host: "host".into(),
                pid: None,
                started_at: 1,
                heartbeat_at: 2,
                status: "idle".into(),
                provider: Some("faux".into()),
                model: None,
                daily_pr_limit: Some(5),
                repo_path: Some(repo_path.display().to_string()),
                service_unit: None,
                config_json: serde_json::json!({ "skills": skills }).to_string(),
            })
            .expect("runner");
    }

    fn write_skill(repo: &std::path::Path, name: &str, version: u32) {
        let dir = repo.join(skills::REPO_SKILLS_DIR);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join(format!("{name}.md")),
            format!(
                "---\nname: {name}\nversion: {version}\ndescription: repo skill\nlanguages: rust\n---\nBODY\n"
            ),
        )
        .expect("write");
    }

    #[test]
    fn catalog_marks_effective_override_and_allowlist() {
        let (_dir, store) = store();
        let repo_dir = tempfile::tempdir().expect("repo");
        write_skill(repo_dir.path(), "rust-cargo", 7);
        seed_runner(
            &store,
            "example.com/app",
            repo_dir.path(),
            &["rust-cargo", "missing"],
        );

        let body = build_response(&store, "example.com/app").expect("response");
        assert_eq!(body.selection_mode, "pinned");
        assert_eq!(body.allowlist, vec!["rust-cargo", "missing"]);
        let builtin = body
            .skills
            .iter()
            .find(|skill| skill.name == "rust-cargo" && skill.source == "builtin")
            .expect("builtin shadowed entry");
        assert!(!builtin.effective);
        let repo = body
            .skills
            .iter()
            .find(|skill| skill.name == "rust-cargo" && skill.source == "repo")
            .expect("repo override");
        assert!(repo.effective);
        assert!(repo.overrides_builtin);
        assert!(repo.allowlisted);
        assert_eq!(repo.version, 7);
        assert!(repo.body.contains("BODY"));
        assert!(body
            .warnings
            .iter()
            .any(|warning| warning.contains("missing")));
    }

    #[test]
    fn no_runner_still_returns_builtins_with_a_warning() {
        let (_dir, store) = store();
        let body = build_response(&store, "example.com/app").expect("response");
        assert_eq!(body.selection_mode, "auto");
        assert!(body.allowlist.is_empty());
        assert!(!body.skills.is_empty());
        assert!(body.skills.iter().all(|skill| skill.source == "builtin"));
        assert!(body
            .warnings
            .iter()
            .any(|warning| warning.contains("运行器")));
    }

    #[test]
    fn bad_repo_skill_is_reported_not_fatal() {
        let (_dir, store) = store();
        let repo_dir = tempfile::tempdir().expect("repo");
        let dir = repo_dir.path().join(skills::REPO_SKILLS_DIR);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("bad.md"), "not a skill").expect("write");
        seed_runner(&store, "example.com/app", repo_dir.path(), &[]);

        let body = build_response(&store, "example.com/app").expect("response");
        assert!(body
            .warnings
            .iter()
            .any(|warning| warning.contains("bad.md")));
        assert!(body.skills.iter().all(|skill| skill.source == "builtin"));
    }

    mod routes {
        use std::sync::Arc;

        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::Router;
        use shanhaijing::Store;
        use tower::ServiceExt;

        use crate::serve::{build_app, AppState};

        fn test_app() -> (tempfile::TempDir, Router) {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("skills.db");
            let store = Arc::new(Store::open(&db_path).expect("open"));
            let state = AppState {
                store,
                ui_token: Arc::new(None),
                ingest_token: Arc::new(None),
                session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
                limiter: Arc::new(crate::session::LoginLimiter::new()),
                started_at_ms: crate::now_ms(),
                env_path: dir.path().join("nuwa.env"),
                db_path,
                cookie_secure: false,
                cookie_secure_never: false,
                serve_unit: "nuwa-serve".to_string(),
                runner_units: vec!["nuwa-work".to_string()],
            };
            (dir, build_app(state))
        }

        #[tokio::test]
        async fn encoded_repo_path_returns_the_builtin_catalog() {
            let (_dir, app) = test_app();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/projects/example.com%2Fapp/skills")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
            assert_eq!(value["repo"], "example.com/app");
            assert_eq!(value["selection_mode"], "auto");
            assert!(!value["skills"].as_array().expect("skills").is_empty());
            assert!(value["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning.as_str().unwrap_or_default().contains("运行器")));
        }
    }
}
