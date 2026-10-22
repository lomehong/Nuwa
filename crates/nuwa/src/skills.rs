//! 版本化修复技能：仓库可复用的排查/修复路径，按语言与失败命令确定性选择后
//! 注入 runner 系统提示。
//!
//! 技能是纯文本（Markdown + frontmatter），来源有两处：
//! - 内置：编译期嵌入 `assets/skills/*.md`；
//! - 仓库：`<repo>/.nuwa/skills/*.md`，同名时覆盖内置版本。
//!
//! 选择必须是确定性的：同一问题单每次都拿到同一组技能，且总长度有上限。

use std::path::Path;

use anyhow::{bail, Context};
use rust_embed::RustEmbed;

use crate::issue::Issue;

pub const REPO_SKILLS_DIR: &str = ".nuwa/skills";
/// 单次注入的技能数上限。
pub const MAX_SKILLS: usize = 3;
const MAX_BODY_CHARS: usize = 3_000;
const MAX_SECTION_CHARS: usize = 8_000;

#[derive(RustEmbed)]
#[folder = "assets/skills/"]
struct BuiltinSkills;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub version: u32,
    pub description: String,
    /// 适用的语言（小写）；空表示不限。
    pub languages: Vec<String>,
    /// 命中失败命令（子串、小写）的关键词；空表示不限。
    pub matches: Vec<String>,
    pub body: String,
    /// `builtin` 或仓库内的相对路径，用于诊断。
    pub origin: String,
}

/// 解析一个技能文件。frontmatter 只支持本模块用到的键，未知键直接报错，
/// 免得拼写错误静默失效。
pub fn parse(text: &str, origin: &str) -> anyhow::Result<Skill> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        bail!("{origin}: missing frontmatter (first line must be `---`)");
    }
    let mut name = None;
    let mut version = 1u32;
    let mut description = None;
    let mut languages = Vec::new();
    let mut matches = Vec::new();
    let mut body = String::new();
    let mut closed = false;
    for line in lines {
        if !closed {
            if line.trim() == "---" {
                closed = true;
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (key, value) = trimmed
                .split_once(':')
                .with_context(|| format!("{origin}: frontmatter line without `:` — `{trimmed}`"))?;
            let value = unquote(value.trim());
            match key.trim() {
                "name" => name = Some(value),
                "version" => {
                    version = value
                        .parse()
                        .with_context(|| format!("{origin}: version must be an integer"))?
                }
                "description" => description = Some(value),
                "languages" => languages = split_list(&value),
                "matches" => matches = split_list(&value),
                other => bail!("{origin}: unknown frontmatter key `{other}`"),
            }
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !closed {
        bail!("{origin}: unterminated frontmatter");
    }
    let name = name.context("name is required")?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("{origin}: name must be lowercase kebab-case");
    }
    if version == 0 {
        bail!("{origin}: version must be >= 1");
    }
    let description = description.context("description is required")?;
    if description.trim().is_empty() {
        bail!("{origin}: description must not be empty");
    }
    let body = body.trim().to_string();
    if body.is_empty() {
        bail!("{origin}: body must not be empty");
    }
    Ok(Skill {
        name,
        version,
        description,
        languages: languages.into_iter().map(|v| v.to_lowercase()).collect(),
        matches: matches.into_iter().map(|v| v.to_lowercase()).collect(),
        body,
        origin: origin.to_string(),
    })
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

/// 内置技能。解析失败是构建缺陷：返回错误，由调用方与测试大声暴露。
pub fn builtin() -> anyhow::Result<Vec<Skill>> {
    let mut skills = Vec::new();
    let mut names: Vec<_> = BuiltinSkills::iter().map(|name| name.to_string()).collect();
    names.sort();
    for name in names {
        let Some(file) = BuiltinSkills::get(&name) else {
            continue;
        };
        let text = std::str::from_utf8(file.data.as_ref())
            .with_context(|| format!("builtin skill {name} is not UTF-8"))?;
        skills.push(parse(text, &format!("builtin:{name}"))?);
    }
    Ok(skills)
}

/// 仓库技能。坏文件只跳过并给出告警——一个技能文件写错不应让 runner 停摆；
/// `nuwa check` 会把同样的告警升级为错误。
pub fn repo_skills(repo: &Path) -> (Vec<Skill>, Vec<String>) {
    let dir = repo.join(REPO_SKILLS_DIR);
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (skills, warnings);
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();
    for path in paths {
        let origin = path
            .strip_prefix(repo)
            .unwrap_or(&path)
            .display()
            .to_string();
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|text| parse(&text, &origin))
        {
            Ok(skill) => skills.push(skill),
            Err(error) => warnings.push(format!("{error:#}")),
        }
    }
    (skills, warnings)
}

/// 合并内置与仓库技能：同名仓库技能覆盖内置版本。
pub fn all(repo: &Path) -> (Vec<Skill>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut skills = match builtin() {
        Ok(skills) => skills,
        Err(error) => {
            warnings.push(format!("builtin skills unavailable: {error:#}"));
            Vec::new()
        }
    };
    let (repo_skills, repo_warnings) = repo_skills(repo);
    warnings.extend(repo_warnings);
    for skill in repo_skills {
        if let Some(existing) = skills.iter_mut().find(|s| s.name == skill.name) {
            *existing = skill;
        } else {
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    (skills, warnings)
}

pub struct Selection {
    /// 实际注入的技能（`name@version`），供决策 trace 记录。
    pub used: Vec<String>,
    /// 追加到系统提示的段落；没有命中时为空。
    pub section: String,
    pub warnings: Vec<String>,
}

/// 选择注入哪些技能：
/// - `allowlist` 非空 = 仓库显式点名，按点名顺序，找不到的名字告警；
/// - 否则按语言（精确命中 2 分）与失败命令关键词（命中 1 分）打分，
///   只取正分，同分按名字排序，最多 [`MAX_SKILLS`] 个。
pub fn select(repo: &Path, issue: &Issue, allowlist: &[String]) -> Selection {
    let (skills, mut warnings) = all(repo);
    let language = issue.language.to_lowercase();
    let command = issue.failing_check.command.to_lowercase();

    let mut chosen: Vec<&Skill> = Vec::new();
    if allowlist.is_empty() {
        let mut scored: Vec<(u32, &Skill)> = skills
            .iter()
            .filter_map(|skill| {
                let mut score = 0;
                if !skill.languages.is_empty()
                    && skill
                        .languages
                        .iter()
                        .any(|lang| lang == "*" || *lang == language)
                {
                    score += 2;
                }
                if skill.matches.iter().any(|needle| command.contains(needle)) {
                    score += 1;
                }
                (score > 0).then_some((score, skill))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        chosen = scored
            .into_iter()
            .take(MAX_SKILLS)
            .map(|(_, s)| s)
            .collect();
    } else {
        for name in allowlist {
            if chosen.iter().any(|skill| skill.name == *name) {
                continue;
            }
            match skills.iter().find(|skill| &skill.name == name) {
                Some(skill) => chosen.push(skill),
                None => warnings.push(format!("skills: unknown skill `{name}`")),
            }
        }
    }
    render(chosen, warnings)
}

fn render(chosen: Vec<&Skill>, mut warnings: Vec<String>) -> Selection {
    if chosen.is_empty() {
        return Selection {
            used: Vec::new(),
            section: String::new(),
            warnings,
        };
    }
    let mut section = String::from(
        "\nREPAIR SKILLS (selected deterministically for this issue; follow them unless the repository evidence contradicts them):\n",
    );
    let mut used = Vec::new();
    for skill in chosen {
        let body: String = skill.body.chars().take(MAX_BODY_CHARS).collect();
        let block = format!(
            "\n--- skill {}@{} — {} ---\n{}\n",
            skill.name, skill.version, skill.description, body
        );
        if section.chars().count() + block.chars().count() > MAX_SECTION_CHARS {
            warnings.push(format!(
                "skills: section cap reached, skipped `{name}`",
                name = skill.name
            ));
            break;
        }
        section.push_str(&block);
        used.push(format!("{}@{}", skill.name, skill.version));
    }
    if used.is_empty() {
        section.clear();
    }
    Selection {
        used,
        section,
        warnings,
    }
}

/// `nuwa check`: 解析仓库技能并校验点名列表。返回 (错误, 告警)。
pub fn validate(repo: &Path, allowlist: &[String]) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    match builtin() {
        Ok(skills) => {
            for skill in &skills {
                if skill.version == 0 {
                    errors.push(format!("builtin skill `{}` has version 0", skill.name));
                }
            }
        }
        Err(error) => errors.push(format!("builtin skills unavailable: {error:#}")),
    }
    let (_, repo_warnings) = repo_skills(repo);
    // 运行时会跳过坏文件；check 是提交前的门，必须升级为错误。
    errors.extend(repo_warnings);
    let (all_skills, _) = all(repo);
    for name in allowlist {
        if !all_skills.iter().any(|skill| &skill.name == name) {
            warnings.push(format!(
                "skills: unknown skill `{name}` (listed in nuwa.repo.json)"
            ));
        }
    }
    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    fn issue(language: &str, command: &str) -> Issue {
        Issue {
            fingerprint: "fp".into(),
            title: "boom".into(),
            language: language.into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: String::new(),
            log_window: Vec::new(),
            prior_context: String::new(),
            failing_check: FailingCheck {
                command: command.into(),
                target: None,
                timeout_secs: 60,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
        }
    }

    fn write_skill(repo: &Path, file: &str, text: &str) {
        let dir = repo.join(REPO_SKILLS_DIR);
        std::fs::create_dir_all(&dir).expect("skills dir");
        std::fs::write(dir.join(file), text).expect("write skill");
    }

    const SAMPLE: &str = "---\nname: rust-cargo\nversion: 2\ndescription: cargo 失败\nlanguages: rust\nmatches: cargo, rustc\n---\nRead the first error.\n";

    #[test]
    fn parses_frontmatter_and_body() {
        let skill = parse(SAMPLE, "test").expect("parse");
        assert_eq!(skill.name, "rust-cargo");
        assert_eq!(skill.version, 2);
        assert_eq!(skill.languages, vec!["rust"]);
        assert_eq!(skill.matches, vec!["cargo", "rustc"]);
        assert_eq!(skill.body, "Read the first error.");
    }

    #[test]
    fn rejects_unknown_keys_and_unterminated_frontmatter() {
        let bad_key = "---\nname: x\ndescription: d\nlanguagse: rust\n---\nbody\n";
        assert!(parse(bad_key, "t").is_err());
        let unterminated = "---\nname: x\ndescription: d\nbody\n";
        assert!(parse(unterminated, "t").is_err());
    }

    #[test]
    fn builtin_skills_parse_and_are_versioned() {
        let skills = builtin().expect("builtin parse");
        assert!(!skills.is_empty());
        for skill in skills {
            assert!(skill.version >= 1, "{} must be versioned", skill.name);
        }
    }

    #[test]
    fn selects_by_language_and_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let selection = select(dir.path(), &issue("rust", "cargo test --workspace"), &[]);
        assert!(selection
            .used
            .iter()
            .any(|name| name.starts_with("rust-cargo@")));
        let none = select(dir.path(), &issue("elixir", "mix test"), &[]);
        assert!(none.used.is_empty());
        assert!(none.section.is_empty());
    }

    #[test]
    fn repo_skill_overrides_builtin_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(
            dir.path(),
            "rust-cargo.md",
            "---\nname: rust-cargo\nversion: 9\ndescription: local override\nlanguages: rust\n---\nLOCAL BODY\n",
        );
        let selection = select(dir.path(), &issue("rust", "cargo test"), &[]);
        assert!(selection.used.contains(&"rust-cargo@9".to_string()));
        assert!(selection.section.contains("LOCAL BODY"));
    }

    #[test]
    fn allowlist_pins_order_and_warns_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let allowlist = vec!["rust-cargo".to_string(), "does-not-exist".to_string()];
        let selection = select(dir.path(), &issue("go", "go test"), &allowlist);
        assert_eq!(selection.used, vec!["rust-cargo@1".to_string()]);
        assert!(selection
            .warnings
            .iter()
            .any(|warning| warning.contains("does-not-exist")));
    }

    #[test]
    fn check_validation_reports_bad_files_and_unknown_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (errors, warnings) = validate(dir.path(), &[]);
        assert!(errors.is_empty(), "builtins must be valid: {errors:?}");
        assert!(warnings.is_empty(), "{warnings:?}");

        write_skill(dir.path(), "bad.md", "---\nname: bad\nbroken\n---\nbody\n");
        let (errors, warnings) = validate(dir.path(), &["missing".to_string()]);
        assert!(
            errors.iter().any(|error| error.contains("bad.md")),
            "runtime skips bad files, check must fail: {errors:?}"
        );
        assert!(
            warnings.iter().any(|warning| warning.contains("missing")),
            "{warnings:?}"
        );
    }

    #[test]
    fn section_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let big = "x".repeat(MAX_BODY_CHARS * 4);
        write_skill(
            dir.path(),
            "huge.md",
            &format!("---\nname: huge\nversion: 1\ndescription: d\nlanguages: rust\n---\n{big}\n"),
        );
        let selection = select(
            dir.path(),
            &issue("rust", "cargo test"),
            &["huge".to_string()],
        );
        assert!(selection.section.chars().count() <= MAX_SECTION_CHARS + 200);
        assert!(selection.used.contains(&"huge@1".to_string()));
    }
}
