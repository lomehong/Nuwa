//! Signal → stable fingerprint（归一化堆栈 + 错误类）。
//!
//! The contract: the same bug reporting twice with different addresses, PIDs
//! or line-number drift must produce the same fingerprint; a different bug in
//! the same file must not. Normalization is deliberately conservative —
//! numbers and hex addresses fold to `N`/`0xN`, everything else is kept.

use sha2::{Digest, Sha256};

/// Parse + normalize a stack trace for `language`, returning
/// `(normalized_error_class, top_frames)`. Unknown languages take the generic
/// fallback (first line, normalized).
pub fn parse(language: &str, stack: &str) -> (String, Vec<String>) {
    match language {
        "go" => parse_go(stack),
        "java" => parse_java(stack),
        "rust" => parse_rust(stack),
        _ => parse_generic(stack),
    }
}

/// Fingerprint = SHA-256 over `lang|error_class|frame|frame|…`.
pub fn compute(language: &str, message: Option<&str>, stack: &str) -> String {
    let (error_class, frames) = parse(language, stack);
    let error_class = match message {
        // An explicit headline (e.g. the first log line) wins when present.
        Some(m) if !m.trim().is_empty() => normalize_text(m),
        _ => error_class,
    };
    let mut hasher = Sha256::new();
    hasher.update(language.as_bytes());
    hasher.update(b"|");
    hasher.update(error_class.as_bytes());
    for frame in frames.iter().take(3) {
        hasher.update(b"|");
        hasher.update(frame.as_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    // Long, but traceable; truncate to 16 bytes = 32 hex chars — collision
    // risk for a single-owner fleet is negligible.
    hex[..32].to_string()
}

/// Fold volatile parts: hex addresses → `0xN`, decimal runs → `N`.
pub fn normalize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'0' && i + 1 < bytes.len() && bytes[i + 1] == b'x' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 {
                out.push_str("0xN");
                i = j;
                continue;
            }
        }
        if bytes[i].is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            out.push('N');
            continue;
        }
        out.push(s[i..].chars().next().expect("utf8 boundary"));
        i += s[i..].chars().next().expect("utf8 boundary").len_utf8();
    }
    out
}

fn push_frame(frames: &mut Vec<String>, func: &str, file_part: &str) {
    // `\t/app/calc.go:17 +0x8c` → basename without line/col.
    let file = file_part
        .split([':', ' '])
        .find(|seg| seg.contains('.'))
        .unwrap_or(file_part);
    let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
    frames.push(format!("{func}@{file}"));
}

/// Go goroutine trace:
/// ```text
/// panic: runtime error: index out of range [3] with length 3
///
/// goroutine 1 [running]:
/// example.com/spike-target.Average(...)
/// 	/app/calc.go:17 +0x8c
/// created by testing.(*T).Run in goroutine 1
/// ```
fn parse_go(stack: &str) -> (String, Vec<String>) {
    let mut error_class = String::new();
    let mut frames = Vec::new();
    let mut pending_func: Option<String> = None;
    for line in stack.lines() {
        let line = line.trim_end();
        if let Some(msg) = line.strip_prefix("panic: ") {
            if error_class.is_empty() {
                error_class = normalize_text(msg);
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("goroutine ") {
            continue;
        }
        if trimmed.starts_with("created by ") {
            break;
        }
        if line.starts_with('\t') {
            if trimmed.contains(".go:") {
                if let Some(func) = pending_func.take() {
                    if !is_go_noise(&func) {
                        push_frame(&mut frames, &func, trimmed);
                    }
                }
            }
            // other indented lines (call args) are ignored
        } else if trimmed.contains('(') {
            // `pkg.Func(args...)` → keep `pkg.Func` only.
            pending_func = Some(trimmed.split('(').next().unwrap_or(trimmed).to_string());
        }
    }
    if error_class.is_empty() {
        error_class = parse_generic(stack).0;
    }
    (error_class, frames)
}

fn is_go_noise(func: &str) -> bool {
    func.starts_with("runtime.")
        || func.starts_with("testing.")
        || func.starts_with("reflect.")
}

/// Java exception chain: first `…Exception/…Error:` line + `at` frames,
/// skipping JDK internals.
fn parse_java(stack: &str) -> (String, Vec<String>) {
    let mut error_class = String::new();
    let mut frames = Vec::new();
    for line in stack.lines() {
        let trimmed = line.trim();
        if error_class.is_empty()
            && (trimmed.contains("Exception") || trimmed.contains("Error"))
            && trimmed.contains(": ")
        {
            error_class = normalize_text(trimmed);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("at ") {
            // `com.acme.calc.Calculator.average(Calculator.java:17)`
            let Some(open) = rest.find('(') else { continue };
            let func = &rest[..open];
            let file_part = rest[open + 1..].trim_end_matches(')');
            let skip = func.starts_with("java.")
                || func.starts_with("javax.")
                || func.starts_with("jdk.")
                || func.starts_with("sun.")
                || func.starts_with("org.junit")
                || func.starts_with("com.intellij");
            if !skip {
                push_frame(&mut frames, func, file_part);
            }
        }
        if frames.len() >= 5 {
            break;
        }
    }
    (error_class, frames)
}

/// Rust panic:
/// ```text
/// thread 'main' panicked at src/calc.rs:17:9:
/// index out of range: the len is 3 but the index is 3
/// ```
/// Backtrace frames (`N: pkg::fn` / `at file:line`) when present.
fn parse_rust(stack: &str) -> (String, Vec<String>) {
    let mut error_class = String::new();
    let mut frames = Vec::new();
    let mut pending_func: Option<String> = None;
    for line in stack.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("thread '") {
            if let Some(at) = rest.find("' panicked at ") {
                let loc = &rest[at + "' panicked at ".len()..];
                let loc = loc.trim_end_matches(':');
                let file = loc
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(loc)
                    .split(':')
                    .next()
                    .unwrap_or(loc);
                frames.push(format!("<panic>@{file}"));
            }
            continue;
        }
        if error_class.is_empty() {
            if trimmed.is_empty() || trimmed.starts_with("stack backtrace:") {
                continue;
            }
            error_class = normalize_text(trimmed);
            continue;
        }
        if trimmed.starts_with("at ") {
            if let Some(func) = pending_func.take() {
                push_frame(&mut frames, &func, trimmed.trim_start_matches("at "));
            }
        } else if let Some(num_colon) = trimmed.strip_suffix(':') {
            if num_colon.chars().next().is_some_and(|c| c.is_ascii_digit()) && trimmed.contains(':')
            {
                // `4: pkg::func` — the func is the part after the ordinal
                if let Some(idx) = trimmed.find(": ") {
                    pending_func = Some(trimmed[idx + 2..].trim().to_string());
                }
            }
        }
        if frames.len() >= 5 {
            break;
        }
    }
    (error_class, frames)
}

fn parse_generic(stack: &str) -> (String, Vec<String>) {
    let first = stack.lines().map(str::trim).find(|l| !l.is_empty());
    (
        first.map(normalize_text).unwrap_or_default(),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_TRACE: &str = "panic: runtime error: index out of range [3] with length 3\n\
        \n\
        goroutine 1 [running]:\n\
        example.com/spike-target.Average(...)\n\
        \t/app/calc.go:17 +0x8c\n\
        example.com/spike-target.main()\n\
        \t/app/calc.go:25 +0x58\n\
        created by testing.(*T).Run in goroutine 1\n\
        \tC:/Program Files/Go/src/testing/testing.go:1851 +0x3f6";

    #[test]
    fn go_trace_is_normalized_and_stable() {
        let a = compute("go", None, GO_TRACE);
        // Same bug, different index/length/addresses/line drift.
        let drifted = GO_TRACE
            .replace("[3] with length 3", "[9] with length 9")
            .replace("calc.go:17", "calc.go:21")
            .replace("+0x8c", "+0x91");
        let b = compute("go", None, &drifted);
        assert_eq!(a, b, "volatile parts must not change the fingerprint");
    }

    #[test]
    fn different_error_is_different_fingerprint() {
        let other = GO_TRACE.replace("index out of range", "nil pointer dereference");
        assert_ne!(compute("go", None, GO_TRACE), compute("go", None, &other));
    }

    #[test]
    fn go_frames_skip_runtime_and_testing() {
        let (_, frames) = parse("go", GO_TRACE);
        assert_eq!(frames.len(), 2, "runtime/testing frames skipped: {frames:?}");
        assert!(frames[0].starts_with("example.com/spike-target.Average@calc.go"));
        assert!(frames[1].starts_with("example.com/spike-target.main@calc.go"));
    }

    #[test]
    fn java_chain_keeps_app_frames_only() {
        let trace = "java.lang.IndexOutOfBoundsException: Index 3 out of bounds for length 3\n\
            \tat jdk.internal.reflect.DirectMethodHandleAccessor.invoke(DirectMethodHandleAccessor.java:103)\n\
            \tat com.acme.calc.Calculator.average(Calculator.java:17)\n\
            \tat com.acme.calc.Main.run(Main.java:25)";
        let (class, frames) = parse("java", trace);
        assert!(class.starts_with("java.lang.IndexOutOfBoundsException: Index N"));
        assert_eq!(frames.len(), 2);
        assert!(frames[0].starts_with("com.acme.calc.Calculator.average@Calculator.java"));
    }

    #[test]
    fn rust_panic_uses_header_and_message() {
        let trace = "thread 'main' panicked at src/calc.rs:17:9:\n\
            index out of range: the len is 3 but the index is 3\n\
            stack backtrace:\n\
            0: nuwa::repair::run\n\
             at .\\crates\\nuwa\\src\\repair.rs:99:5";
        let (class, frames) = parse("rust", trace);
        assert!(class.contains("the len is N but the index is N"), "{class}");
        assert!(frames[0].contains("calc.rs"), "{frames:?}");
    }

    #[test]
    fn explicit_message_wins_over_derived() {
        let derived = compute("go", None, GO_TRACE);
        let explicit = compute("go", Some("reporter crashed on window N"), GO_TRACE);
        assert_ne!(derived, explicit);
    }

    #[test]
    fn unknown_language_falls_back() {
        let (class, frames) = parse("cobol", "SOME ERROR 42 happened");
        assert_eq!(class, "SOME ERROR N happened");
        assert!(frames.is_empty());
    }
}
