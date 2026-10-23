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

/// Fingerprint = SHA-256 over `repo|lang|error_class|frame|frame|…`.
///
/// The repo is part of the identity: two repositories that happen to share a
/// fully-qualified symbol path and error message (forks, vendored copies) are
/// still two separate bugs in two separate codebases, and must not collapse
/// into one issue row that only one runner can claim. `fingerprint` stays the
/// single primary key, so URLs/runs/events are unchanged — the value simply
/// becomes repo-qualified.
///
/// Contract with the 青鸟 SDK: `panicFingerprint` must hash the same layout
/// (`repo|lang|error_class|frames…`, repo first) so client-side throttling and
/// server-side aggregation agree. The server always recomputes and ignores any
/// client value, so a lagging SDK only affects its own throttle key, never
/// aggregation correctness.
pub fn compute(repo: &str, language: &str, message: Option<&str>, stack: &str) -> String {
    let (error_class, frames) = parse(language, stack);
    let error_class = match message {
        // An explicit headline (e.g. the first log line) wins when present.
        Some(m) if !m.trim().is_empty() => normalize_text(m),
        _ => error_class,
    };
    let mut hasher = Sha256::new();
    hasher.update(repo.as_bytes());
    hasher.update(b"|");
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
///     /app/calc.go:17 +0x8c
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
            // `pkg.Func(args...)` → keep `pkg.Func`; `pkg.(*Recv).Method(args)`
            // → keep `pkg.(*Recv).Method` (a pointer receiver must not be
            // truncated at its own paren, or Average and Median on the same
            // type would collapse into one fingerprint).
            pending_func = Some(go_func_symbol(trimmed).to_string());
        }
    }
    if error_class.is_empty() {
        error_class = parse_generic(stack).0;
    }
    (error_class, frames)
}

/// Strip a Go frame's trailing argument list, keeping the full function symbol.
/// The argument list is the final balanced paren group; a pointer receiver's
/// own parens are nested before it, so scanning back to the matching `(` of the
/// terminal `)` preserves `pkg.(*Recv).Method`.
fn go_func_symbol(line: &str) -> &str {
    if !line.ends_with(')') {
        // No argument list to strip (`pkg.Func` with no parens, or a frame that
        // does not end in a call) — keep it whole.
        return line;
    }
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().rev() {
        match b {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return &line[..i];
                }
            }
            _ => {}
        }
    }
    line
}

fn is_go_noise(func: &str) -> bool {
    func.starts_with("runtime.") || func.starts_with("testing.") || func.starts_with("reflect.")
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
        if let Some(file_part) = trimmed.strip_prefix("at ") {
            if let Some(func) = pending_func.take() {
                push_frame(&mut frames, &func, file_part);
            }
        } else if let Some((ordinal, func)) = trimmed.split_once(": ") {
            if !ordinal.is_empty() && ordinal.bytes().all(|b| b.is_ascii_digit()) {
                // Release backtraces may omit locations entirely. A new ordinal
                // completes the preceding valid function-only frame.
                if let Some(func) = pending_func.take() {
                    frames.push(func);
                }
                let func = func.trim();
                // As with Go/Java, runtime and test harness frames must not
                // consume the limited application-frame budget. Clear pending
                // even for noise, so its location cannot attach to an app frame.
                let noise = func.starts_with("std::")
                    || func.starts_with("core::")
                    || func.starts_with("alloc::")
                    || func.starts_with("test::")
                    || func.starts_with("__rustc::")
                    || func == "rust_begin_unwind";
                pending_func = (!func.is_empty() && !noise).then(|| func.to_string());
            }
        }
        if frames.len() >= 5 {
            break;
        }
    }
    if frames.len() < 5 {
        if let Some(func) = pending_func {
            frames.push(func);
        }
    }
    (error_class, frames)
}

fn parse_generic(stack: &str) -> (String, Vec<String>) {
    let first = stack.lines().map(str::trim).find(|l| !l.is_empty());
    (first.map(normalize_text).unwrap_or_default(), Vec::new())
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

    /// The repo the SDK golden test pins; both sides must hash this same value.
    const GOLDEN_REPO: &str = "example.com/spike-target";

    #[test]
    fn go_trace_is_normalized_and_stable() {
        let a = compute(GOLDEN_REPO, "go", None, GO_TRACE);
        // Same bug, different index/length/addresses/line drift.
        let drifted = GO_TRACE
            .replace("[3] with length 3", "[9] with length 9")
            .replace("calc.go:17", "calc.go:21")
            .replace("+0x8c", "+0x91");
        let b = compute(GOLDEN_REPO, "go", None, &drifted);
        assert_eq!(a, b, "volatile parts must not change the fingerprint");
    }

    #[test]
    fn different_repo_is_different_fingerprint() {
        // The same stack in two repositories is two bugs, not one: a shared
        // fingerprint would let repo A's issue swallow repo B's reports and
        // leave B's runner with nothing to claim.
        let a = compute("example.com/acme/a", "go", None, GO_TRACE);
        let b = compute("example.com/acme/b", "go", None, GO_TRACE);
        assert_ne!(a, b, "repo must be part of the issue identity");
    }

    #[test]
    fn go_pointer_receiver_methods_are_distinct() {
        // `pkg.(*Calc).Average` and `pkg.(*Calc).Median` must not collapse: the
        // old `split('(')` truncated both to `pkg.`.
        let avg = "panic: boom\n\ngoroutine 1 [running]:\n\
            example.com/app.(*Calc).Average(0x1)\n\t/app/calc.go:17 +0x8c\n";
        let med = "panic: boom\n\ngoroutine 1 [running]:\n\
            example.com/app.(*Calc).Median(0x1)\n\t/app/calc.go:17 +0x8c\n";
        assert_ne!(
            compute(GOLDEN_REPO, "go", None, avg),
            compute(GOLDEN_REPO, "go", None, med),
            "distinct pointer-receiver methods must fingerprint differently"
        );
        let (_, frames) = parse("go", avg);
        assert_eq!(
            frames[0], "example.com/app.(*Calc).Average@calc.go",
            "{frames:?}"
        );
    }

    #[test]
    fn different_error_is_different_fingerprint() {
        let other = GO_TRACE.replace("index out of range", "nil pointer dereference");
        assert_ne!(
            compute(GOLDEN_REPO, "go", None, GO_TRACE),
            compute(GOLDEN_REPO, "go", None, &other)
        );
    }

    #[test]
    fn go_frames_skip_runtime_and_testing() {
        let (_, frames) = parse("go", GO_TRACE);
        assert_eq!(
            frames.len(),
            2,
            "runtime/testing frames skipped: {frames:?}"
        );
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
    fn rust_standard_frames_distinguish_functions_and_ignore_line_drift() {
        let trace = "thread 'main' panicked at src/calc.rs:17:9:\nboom\nstack backtrace:\n\
            0: app::average\n at src/calc.rs:17:9\n\
            1: app::run\n at src/main.rs:25:1";
        assert_eq!(
            parse("rust", trace).1,
            [
                "<panic>@calc.rs",
                "app::average@calc.rs",
                "app::run@main.rs"
            ]
        );
        let median = trace.replace("app::average", "app::median");
        assert_ne!(
            compute(GOLDEN_REPO, "rust", Some("boom"), trace),
            compute(GOLDEN_REPO, "rust", Some("boom"), &median)
        );
        let drifted = trace
            .replace(":17:9", ":101:2")
            .replace(":25:1", ":99:3")
            .replace("0: app", "12: app")
            .replace("1: app", "13: app");
        assert_eq!(
            compute(GOLDEN_REPO, "rust", None, trace),
            compute(GOLDEN_REPO, "rust", None, &drifted)
        );
    }

    #[test]
    fn review_rust_function_only_frames_distinguish_same_message() {
        let trace = "thread 'main' panicked at src/calc.rs:17:9:\nboom\nstack backtrace:\n\
            0: std::panicking::begin_panic_handler\n\
            1: app::average\n\
            2: app::run";
        assert_ne!(
            compute(GOLDEN_REPO, "rust", Some("boom"), trace),
            compute(
                GOLDEN_REPO,
                "rust",
                Some("boom"),
                &trace.replace("app::average", "app::median")
            )
        );
        assert_eq!(
            parse("rust", trace).1,
            ["<panic>@calc.rs", "app::average", "app::run"]
        );
    }

    #[test]
    fn review_rust_mixed_locations_flush_pending_at_next_frame_and_eof() {
        let trace = "boom\nstack backtrace:\n\
            0: app::no_location\n\
            1: core::panicking::panic_fmt\n at library/core/src/panicking.rs:1:1\n\
            2: app::with_location\n at src/calc.rs:17:9\n\
            3: app::last";
        assert_eq!(
            parse("rust", trace).1,
            [
                "app::no_location",
                "app::with_location@calc.rs",
                "app::last"
            ]
        );
        assert_eq!(parse("rust", "boom\n0: app::only").1, ["app::only"]);
    }

    #[test]
    fn review_rust_function_only_frames_respect_frame_budget() {
        let trace =
            "boom\n0: app::a\n1: app::b\n2: app::c\n3: app::d\n4: app::e\n5: app::f\n6: app::g";
        assert_eq!(
            parse("rust", trace).1,
            ["app::a", "app::b", "app::c", "app::d", "app::e"]
        );
    }

    #[test]
    fn rust_runtime_noise_does_not_displace_application_frames() {
        let trace = "thread 'main' panicked at src/calc.rs:17:9:\nboom\nstack backtrace:\n\
            0: std::backtrace_rs::backtrace::libunwind::trace\n at library/std/src/backtrace.rs:1:1\n\
            1: std::panicking::begin_panic_handler\n at library/std/src/panicking.rs:2:1\n\
            2: core::panicking::panic_fmt\n at library/core/src/panicking.rs:3:1\n\
            3: rust_begin_unwind\n at library/std/src/panicking.rs:4:1\n\
            4: __rustc::rust_begin_unwind\n at library/std/src/panicking.rs:5:1\n\
            5: alloc::vec::Vec<T>::push\n at library/alloc/src/vec.rs:6:1\n\
            6: test::run_test\n at library/test/src/lib.rs:7:1\n\
            7: app::average\n at src/calc.rs:17:9\n\
            8: core::ops::function::FnOnce::call_once\n at library/core/src/function.rs:8:1\n\
            9: app::run\n at src/main.rs:25:1";
        assert_eq!(
            parse("rust", trace).1,
            [
                "<panic>@calc.rs",
                "app::average@calc.rs",
                "app::run@main.rs"
            ]
        );
        let other = trace.replace("app::average", "app::median");
        assert_ne!(
            compute(GOLDEN_REPO, "rust", None, trace),
            compute(GOLDEN_REPO, "rust", None, &other)
        );
    }

    #[test]
    fn rust_frame_ordinals_must_be_entirely_numeric() {
        let trace = "thread 'main' panicked at src/calc.rs:1:1:\nboom\n\
            1bad: not::a_frame\n at src/wrong.rs:1\n\
            : not::a_frame\n at src/wrong.rs:2\n\
            12: app::run\n at src/calc.rs:3";
        assert_eq!(
            parse("rust", trace).1,
            ["<panic>@calc.rs", "app::run@calc.rs"]
        );
    }

    #[test]
    fn explicit_message_wins_over_derived() {
        let derived = compute(GOLDEN_REPO, "go", None, GO_TRACE);
        let explicit = compute(
            GOLDEN_REPO,
            "go",
            Some("reporter crashed on window N"),
            GO_TRACE,
        );
        assert_ne!(derived, explicit);
    }

    #[test]
    fn unknown_language_falls_back() {
        let (class, frames) = parse("cobol", "SOME ERROR 42 happened");
        assert_eq!(class, "SOME ERROR N happened");
        assert!(frames.is_empty());
    }

    #[test]
    fn rust_panic_fingerprint_matches_the_server_golden() {
        // Includes the standard numbered application frame, formerly dropped.
        // qingniao-rust must adopt this parser/golden separately; the server
        // recomputes fingerprints, so an older SDK only has a stale throttle key.
        let trace = "thread 'main' panicked at src/calc.rs:17:9:\n\
                     index out of range: the len is 3 but the index is 3\n\
                     stack backtrace:\n\
                     0: app::run\n\
                                 at src/calc.rs:17:9";
        assert_eq!(
            compute("example.com/app", "rust", None, trace),
            "912e5aca6f97d582ae5e7e4b1ab644d6"
        );
    }

    /// The client-side Go panic fingerprint (青鸟 SDK `panicFingerprint`) must
    /// equal the server-side one, otherwise the same panic aggregates under two
    /// different keys depending on who computed it. The SDK pins the same
    /// constant, so a divergence breaks one of the two test suites.
    ///
    /// The repo is now part of the identity (see [`compute`]); the SDK must hash
    /// `repo|lang|error_class|frames…` with the same repo string to stay aligned.
    #[test]
    fn go_panic_fingerprint_matches_the_sdk_golden() {
        let trace = "panic: runtime error: index out of range [3] with length 3

goroutine 1 [running]:
example.com/spike-target.Average(...)
	/app/calc.go:17 +0x8c
example.com/spike-target.main()
	/app/calc.go:25 +0x58
created by testing.(*T).Run in goroutine 1
	C:/Program Files/Go/src/testing/testing.go:1851 +0x3f6";
        let message = "runtime error: index out of range [3] with length 3";
        assert_eq!(
            compute(GOLDEN_REPO, "go", Some(message), trace),
            "5cc9dae18baa7dac8582fda50dedec8d"
        );
    }
}
