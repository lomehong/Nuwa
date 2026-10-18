#!/usr/bin/env python3
"""Seed a local Nuwa database for the console E2E run (M5 Task 8).

Python 3 standard library only. The database is created/migrated by
``<bin> status --db <db>`` first, then filled with deterministic fixtures:

* issues across every lifecycle state (64-char SHA-256 fingerprints),
* three runners, four runs, 14 days of events and ``signal_daily`` buckets,
* one real JSONL repair transcript under ``--sessions/<run dir>/<slug>/``.

Example::

    python web/e2e/seed.py --db .superpowers/tmp-m5-e2e/e2e.db \
        --bin ~/.cargo/shared-target/debug/nuwa.exe \
        --sessions .superpowers/tmp-m5-e2e/sessions

``--issues N`` bulk-inserts N synthetic cases (M6 benchmark); only the
default eight carry the hand-written realistic details.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sqlite3
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

DAY_MS = 86_400_000
HOUR_MS = 3_600_000
STATES = ["new", "triggered", "fixing", "pr_open", "merged", "rejected", "escalated"]

# Hand-written fixtures: the default seed is one full lifecycle plus a repeat
# of `new`. Bulk runs cycle these bodies and suffix the title.
FIXTURES = [
    {
        "title": "panic: assignment to entry in nil map",
        "language": "go",
        "repo": "example.com/acme/api",
        "severity": "high",
        "stack": (
            "panic: assignment to entry in nil map\n"
            "goroutine 42 [running]:\n"
            "example.com/acme/api/internal/cache.(*Store).Put(...)\n"
            "\t/srv/acme/api/internal/cache/store.go:87 +0x2f\n"
            "example.com/acme/api/internal/handler.(*Handler).Create(...)\n"
            "\t/srv/acme/api/internal/handler/create.go:112 +0x1d5"
        ),
    },
    {
        "title": "NullPointerException in CheckoutService.total",
        "language": "java",
        "repo": "example.com/acme/shop",
        "severity": "critical",
        "stack": (
            "java.lang.NullPointerException: Cannot invoke "
            '"com.acme.checkout.Cart.items()" because "cart" is null\n'
            "\tat com.acme.checkout.CheckoutService.total(CheckoutService.java:148)\n"
            "\tat com.acme.checkout.CheckoutController.checkout(CheckoutController.java:72)"
        ),
    },
    {
        "title": "thread 'worker-3' panicked at src/queue.rs:184: index out of bounds",
        "language": "rust",
        "repo": "example.com/acme/worker",
        "severity": "high",
        "stack": (
            "thread 'worker-3' panicked at src/queue.rs:184:21:\n"
            "index out of bounds: the len is 5 but the index is 7\n"
            "stack backtrace:\n"
            "   0: acme_worker::queue::drain\n"
            "   1: acme_worker::runtime::poll\n"
            "   2: std::thread::Builder::spawn_unchecked_::{{closure}}"
        ),
    },
    {
        "title": "TypeError: Cannot read properties of undefined (reading 'items')",
        "language": "typescript",
        "repo": "example.com/acme/web",
        "severity": "medium",
        "stack": (
            "TypeError: Cannot read properties of undefined (reading 'items')\n"
            "    at CartPanel.render (src/components/CartPanel.tsx:64:31)\n"
            "    at renderWithHooks (node_modules/react-dom/cjs/react-dom.development.js:16305:18)\n"
            "    at updateFunctionComponent (node_modules/react-dom/cjs/react-dom.development.js:19588:20)"
        ),
    },
    {
        "title": "Segmentation fault in tensor_alloc (heap corruption)",
        "language": "cpp",
        "repo": "example.com/acme/inference",
        "severity": "critical",
        "stack": (
            "Program terminated with signal SIGSEGV, Segmentation fault.\n"
            "#0  0x00007f2c11a4e2d1 in acme::tensor_alloc(unsigned long) () from /opt/acme/libcore.so\n"
            "#1  0x00007f2c11a51b44 in acme::Batch::reserve(unsigned long) () from /opt/acme/libcore.so\n"
            "#2  0x000055d4c12013f9 in main ()"
        ),
    },
    {
        "title": "panic: runtime error: index out of range [7] with length 5",
        "language": "go",
        "repo": "example.com/acme/ledger",
        "severity": "high",
        "stack": (
            "panic: runtime error: index out of range [7] with length 5\n"
            "goroutine 18 [running]:\n"
            "example.com/acme/ledger/internal/batch.(*Window).Apply(...)\n"
            "\t/srv/acme/ledger/internal/batch/window.go:203 +0x2c9\n"
            "example.com/acme/ledger/internal/job.(*Job).Run(...)\n"
            "\t/srv/acme/ledger/internal/job/job.go:88 +0x11a"
        ),
    },
    {
        "title": "FATAL: connection pool exhausted after 30s",
        "language": "python",
        "repo": "example.com/acme/ingest",
        "severity": "medium",
        "stack": (
            "sqlalchemy.exc.TimeoutError: QueuePool limit of size 10 overflow 20 reached, "
            "connection timed out, timeout 30.00\n"
            '  File "/srv/acme/ingest/app/db.py", line 41, in session\n'
            '    return SessionLocal()\n'
            '  File "/srv/acme/ingest/app/worker.py", line 118, in handle_batch'
        ),
    },
    {
        "title": "assertion failed: len <= capacity (buffer overflow in framed codec)",
        "language": "rust",
        "repo": "example.com/acme/gateway",
        "severity": "high",
        "stack": (
            "thread 'main' panicked at src/codec/frame.rs:97:9:\n"
            "assertion failed: len <= capacity\n"
            "stack backtrace:\n"
            "   0: acme_gateway::codec::frame::decode\n"
            "   1: acme_gateway::conn::serve\n"
            "   2: tokio::runtime::task::core::poll"
        ),
    },
]

RUNNERS = [
    {
        "id": "e2e-host-1:acme-api",
        "repo": "example.com/acme/api",
        "host": "e2e-host-1",
        "offset_ms": 5_000,
        "status": "working",
        "phase": "repair",
        "service_unit": "nuwa-work",
        "last_error": None,
    },
    {
        "id": "e2e-host-1:acme-web",
        "repo": "example.com/acme/web",
        "host": "e2e-host-1",
        "offset_ms": 8_000,
        "status": "idle",
        "phase": None,
        "service_unit": "nuwa-work-web",
        "last_error": None,
    },
    {
        "id": "e2e-host-2:acme-worker",
        "repo": "example.com/acme/worker",
        "host": "e2e-host-2",
        "offset_ms": 12_000,
        "status": "error",
        "phase": None,
        "service_unit": "nuwa-work-worker",
        "last_error": "GitHub 502 while opening PR; will retry on next heartbeat",
    },
]


def fingerprint_for(index: int, repo: str, title: str) -> str:
    return hashlib.sha256(f"nuwa-e2e:{repo}:{title}:{index}".encode("utf-8")).hexdigest()


def fixture_for(index: int) -> dict:
    base = FIXTURES[index % len(FIXTURES)]
    title = base["title"]
    if index >= len(FIXTURES):
        title = f"{title} [#{index + 1}]"
    return {
        "title": title,
        "language": base["language"],
        "repo": base["repo"],
        "severity": base["severity"],
        "stack": base["stack"],
    }


def resolve_bin(raw: str) -> str:
    candidate = Path(raw).expanduser()
    if candidate.is_file():
        return str(candidate.resolve())
    found = shutil.which(raw)
    if found:
        return found
    raise SystemExit(f"seed: nuwa binary not found: {raw}")


def ensure_schema(binary: str, db: Path) -> None:
    result = subprocess.run(
        [binary, "status", "--db", str(db)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode != 0:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit(f"seed: `nuwa status --db {db}` failed ({result.returncode})")


def iso_day(days_ago: int) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).strftime("%Y-%m-%d")


def state_detail(state: str, runner_id: str | None) -> str:
    if state == "new":
        return "reported by ingest"
    if state in ("triggered", "fixing"):
        return f"claimed by {runner_id or 'runner'}"
    if state == "pr_open":
        return "pr opened"
    if state == "merged":
        return "merged by owner"
    if state == "rejected":
        return "repair_failed: CI red"
    return "repair_failed: max turns exceeded"


def write_transcript(sessions: Path, run_id: str, repo: str, stack: str, now_ms: int) -> str:
    run_dir = sessions / run_id
    slug = repo.replace("example.com/", "example.com-").replace("/", "-")
    transcript_dir = run_dir / slug
    transcript_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.fromtimestamp(now_ms / 1000, tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = transcript_dir / f"{stamp}_{run_id}.jsonl"

    def entry(seq: int, ts: int, message: dict) -> dict:
        return {"kind": "entry", "type": "message", "seq": seq, "timestamp": ts, "message": message}

    ts = now_ms - 6 * 60_000
    lines = [
        {
            "kind": "header",
            "version": 1,
            "id": run_id,
            "cwd": f"/srv/nuwa/repo/{repo.split('/')[-1]}",
            "timestamp": ts,
        },
        entry(
            1,
            ts,
            {
                "kind": "user",
                "content": (
                    f"Fix the following failure in {repo} and make the failing check pass.\n"
                    f"{stack.splitlines()[0]}"
                ),
            },
        ),
        entry(
            2,
            ts + 2_000,
            {
                "kind": "assistant",
                "content": [
                    {
                        "type": "thinking",
                        "thinking": (
                            "The panic points at the cache store. I will inspect the "
                            "constructor and the batch window before editing."
                        ),
                    },
                    {
                        "type": "text",
                        "text": "Reading the failing module and its callers.",
                    },
                    {
                        "type": "toolCall",
                        "name": "read",
                        "arguments": {"path": "internal/cache/store.go"},
                    },
                ],
            },
        ),
        entry(
            3,
            ts + 3_000,
            {
                "kind": "tool_result",
                "toolName": "read",
                "isError": False,
                "content": [
                    {
                        "type": "text",
                        "text": "package cache\n\nfunc (s *Store) Put(k string, v []byte) {\n\ts.items[k] = v\n}",
                    }
                ],
            },
        ),
        entry(
            4,
            ts + 4_000,
            {
                "kind": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "The map is never initialized. Initializing it in NewStore fixes the nil write.",
                    },
                    {
                        "type": "toolCall",
                        "name": "edit",
                        "arguments": {
                            "path": "internal/cache/store.go",
                            "old_string": "return &Store{}",
                            "new_string": "return &Store{items: make(map[string][]byte)}",
                        },
                    },
                ],
            },
        ),
        entry(
            5,
            ts + 6_000,
            {
                "kind": "tool_result",
                "toolName": "edit",
                "isError": False,
                "content": [{"type": "text", "text": "Updated internal/cache/store.go (1 hunk)."}],
            },
        ),
        entry(
            6,
            ts + 7_000,
            {
                "kind": "assistant",
                "content": [
                    {"type": "text", "text": "Re-running the failing check before delivery."},
                    {
                        "type": "toolCall",
                        "name": "bash",
                        "arguments": {"command": "go test ./internal/cache/...", "timeout": 120},
                    },
                ],
            },
        ),
        entry(
            7,
            ts + 9_000,
            {
                "kind": "tool_result",
                "toolName": "bash",
                "isError": False,
                "content": [{"type": "text", "text": "ok  \texample.com/acme/api/internal/cache\t0.42s"}],
            },
        ),
        entry(
            8,
            ts + 10_000,
            {"kind": "assistant", "content": [{"type": "text", "text": "Check passes. Delivery can proceed."}]},
        ),
    ]
    path.write_text("\n".join(json.dumps(line, ensure_ascii=False) for line in lines) + "\n", encoding="utf-8")
    return str(path)


def seed(db: Path, binary: str, sessions: Path, count: int) -> dict:
    ensure_schema(binary, db)
    sessions.mkdir(parents=True, exist_ok=True)
    now = int(time.time() * 1000)

    issues = []
    for index in range(count):
        fixture = fixture_for(index)
        state = STATES[index % len(STATES)]
        fp = fingerprint_for(index, fixture["repo"], fixture["title"])
        first_seen = now - 14 * DAY_MS + (index % 14) * DAY_MS - (index % 17) * 60_000
        last_seen = now - (index % 14) * DAY_MS - (index % 23) * HOUR_MS
        if last_seen < first_seen:
            last_seen = first_seen + HOUR_MS
        occurrences = 1 + (index * 7) % 120
        pr_url = None
        pr_opened_at = None
        if state in ("pr_open", "merged", "rejected"):
            pr_url = f"https://example.com/acme/{fixture['repo'].split('/')[-1]}/pull/{100 + index}"
            pr_opened_at = last_seen - 18 * HOUR_MS
        owner = None
        if state == "fixing":
            owner = RUNNERS[0]["id"]
        elif state == "pr_open":
            owner = RUNNERS[1]["id"]
        elif state == "escalated":
            owner = RUNNERS[2]["id"]
        issues.append(
            {
                "index": index,
                "fingerprint": fp,
                "title": fixture["title"],
                "language": fixture["language"],
                "repo": fixture["repo"],
                "severity": fixture["severity"],
                "stack": fixture["stack"],
                "state": state,
                "occurrences": occurrences,
                "first_seen": first_seen,
                "last_seen": last_seen,
                "state_changed_at": max(first_seen, last_seen - 2 * HOUR_MS),
                "cooldown_until": now + 6 * HOUR_MS if state in ("new", "triggered", "fixing") else 0,
                "owner": owner,
                "pr_url": pr_url,
                "pr_opened_at": pr_opened_at,
            }
        )

    by_state: dict[str, dict] = {}
    for issue in issues:
        by_state.setdefault(issue["state"], issue)

    conn = sqlite3.connect(db)
    conn.execute("PRAGMA foreign_keys=ON")
    try:
        with conn:
            fingerprints = [issue["fingerprint"] for issue in issues]
            conn.executemany("DELETE FROM events WHERE fingerprint = ?", [(fp,) for fp in fingerprints])
            conn.execute("DELETE FROM events WHERE fingerprint = '__config__'")
            conn.executemany("DELETE FROM signal_daily WHERE fingerprint = ?", [(fp,) for fp in fingerprints])

            conn.executemany(
                """INSERT OR REPLACE INTO issues (
                    fingerprint, title, language, severity, state, occurrences,
                    first_seen, last_seen, cooldown_until, repo, stack_sample,
                    pr_url, pr_opened_at, state_changed_at, owner_runner_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                [
                    (
                        issue["fingerprint"],
                        issue["title"],
                        issue["language"],
                        issue["severity"],
                        issue["state"],
                        issue["occurrences"],
                        issue["first_seen"],
                        issue["last_seen"],
                        issue["cooldown_until"],
                        issue["repo"],
                        issue["stack"],
                        issue["pr_url"],
                        issue["pr_opened_at"],
                        issue["state_changed_at"],
                        issue["owner"],
                    )
                    for issue in issues
                ],
            )

            events = []
            for issue in issues:
                events.append(
                    (
                        issue["fingerprint"],
                        issue["first_seen"],
                        "reported",
                        f"reported occurrence 1/{issue['occurrences']}",
                        issue["repo"],
                        None,
                    )
                )
                if issue["occurrences"] > 1:
                    events.append(
                        (
                            issue["fingerprint"],
                            issue["first_seen"] + (issue["last_seen"] - issue["first_seen"]) // 2,
                            "reported",
                            "reported by ingest",
                            issue["repo"],
                            None,
                        )
                    )
                events.append(
                    (
                        issue["fingerprint"],
                        issue["state_changed_at"],
                        "state_changed",
                        state_detail(issue["state"], issue["owner"]),
                        issue["repo"],
                        "runner" if issue["owner"] else "owner",
                    )
                )
                if issue["state"] == "pr_open":
                    events.append(
                        (issue["fingerprint"], issue["last_seen"], "pr_opened", issue["pr_url"], issue["repo"], "runner")
                    )
                elif issue["state"] == "merged":
                    events.append(
                        (issue["fingerprint"], issue["last_seen"], "merged", issue["pr_url"], issue["repo"], "owner")
                    )
                elif issue["state"] == "rejected":
                    events.append(
                        (
                            issue["fingerprint"],
                            issue["last_seen"],
                            "rejected",
                            "CI failed: go test ./... (exit 1)",
                            issue["repo"],
                            "runner",
                        )
                    )
            config_ts = now - 3 * DAY_MS
            events.append(("__config__", config_ts, "config_changed", "NUWA_INTERVAL", None, "owner"))
            events.append(
                ("__config__", config_ts + 1_000, "restart_requested", "nuwa-serve,nuwa-work", None, "owner")
            )
            conn.executemany(
                "INSERT INTO events (fingerprint, ts, kind, detail, repo, actor) VALUES (?, ?, ?, ?, ?, ?)",
                events,
            )

            conn.executemany(
                "INSERT OR REPLACE INTO signal_daily (fingerprint, repo, day, count) VALUES (?, ?, ?, ?)",
                [
                    (issue["fingerprint"], issue["repo"], iso_day(day), max(1, issue["occurrences"] // (day + 1)))
                    for issue in issues
                    for day in range(3)
                ],
            )

            config_json = json.dumps(
                {
                    "interval_secs": 30,
                    "ci_timeout_secs": 600,
                    "max_turns": 25,
                    "max_tokens": 200_000,
                },
                separators=(",", ":"),
            )
            fixing = by_state.get("fixing")
            conn.executemany(
                """INSERT OR REPLACE INTO runners (
                    id, repo, host, pid, started_at, heartbeat_at, status,
                    current_fingerprint, phase, provider, model, daily_pr_limit,
                    repo_path, service_unit, config_json, last_error
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                [
                    (
                        runner["id"],
                        runner["repo"],
                        runner["host"],
                        4200 + index,
                        now - 8 * HOUR_MS,
                        now - runner["offset_ms"],
                        runner["status"],
                        fixing["fingerprint"] if runner["status"] == "working" and fixing else None,
                        runner["phase"],
                        "zai",
                        "glm-5.3-flash",
                        5,
                        f"/srv/nuwa/repo/{runner['repo'].split('/')[-1]}",
                        runner["service_unit"],
                        config_json,
                        runner["last_error"],
                    )
                    for index, runner in enumerate(RUNNERS)
                ],
            )

            runs = []

            def add_run(issue: dict | None, runner: int, *, finished: bool, phase: str, outcome: str | None,
                        ci: str | None, branch: str | None, session_dir: str | None,
                        error: str | None = None, verification: int | None = None) -> None:
                if issue is None:
                    return
                run_id = f"run-{issue['index']:03d}-{issue['state']}"
                started = issue["last_seen"] - 8 * 60_000
                pr_url = issue["pr_url"]
                runs.append(
                    (
                        run_id,
                        f"h-{issue['index']:03d}",
                        issue["fingerprint"],
                        RUNNERS[runner]["id"],
                        issue["repo"],
                        started,
                        started + 6 * 60_000 if finished else None,
                        phase,
                        outcome,
                        ci,
                        14 if finished else 3,
                        17_500 if finished else 4_200,
                        2_800 if finished else 1_100,
                        "zai",
                        "glm-5.3-flash",
                        "nil map filled before first write" if finished else None,
                        "2 files changed, 18 insertions(+), 4 deletions(-)" if finished else None,
                        branch,
                        hashlib.sha1(f"{run_id}:commit".encode()).hexdigest() if finished else None,
                        pr_url,
                        issue["pr_opened_at"],
                        verification if finished else None,
                        "failing check re-run: ok" if finished else None,
                        session_dir,
                        error,
                    )
                )

            transcript = None
            fix_issue = fixing
            session_dir = None
            if fix_issue is not None:
                run_id = f"run-{fix_issue['index']:03d}-fixing"
                session_dir = str((sessions / run_id).resolve())
                transcript = write_transcript(sessions, run_id, fix_issue["repo"], fix_issue["stack"], now)

            add_run(
                fix_issue,
                0,
                finished=False,
                phase="repair",
                outcome=None,
                ci=None,
                branch=f"agent/fix/{fix_issue['fingerprint'][:16]}" if fix_issue else None,
                session_dir=session_dir,
            )
            pr_issue = by_state.get("pr_open")
            add_run(
                pr_issue,
                1,
                finished=True,
                phase="ci",
                outcome="success",
                ci="success",
                branch=f"agent/fix/{pr_issue['fingerprint'][:16]}" if pr_issue else None,
                session_dir=None,
                verification=1,
            )
            merged_issue = by_state.get("merged")
            add_run(
                merged_issue,
                1,
                finished=True,
                phase="deliver",
                outcome="success",
                ci="success",
                branch=f"agent/fix/{merged_issue['fingerprint'][:16]}" if merged_issue else None,
                session_dir=None,
                verification=1,
            )
            rejected_issue = by_state.get("rejected")
            add_run(
                rejected_issue,
                2,
                finished=True,
                phase="ci",
                outcome="ci_red",
                ci="failure",
                branch=f"agent/fix/{rejected_issue['fingerprint'][:16]}" if rejected_issue else None,
                session_dir=None,
                error="CI failed: go test ./... (exit 1)",
                verification=0,
            )
            conn.executemany(
                """INSERT OR REPLACE INTO runs (
                    id, harness_run_id, fingerprint, runner_id, repo, started_at,
                    finished_at, phase, outcome, ci_verdict, turns, tokens_uncached,
                    tokens_cached, provider, model, root_cause, change_stat, branch,
                    commit_sha, pr_url, pr_opened_at, verification_ok,
                    verification_detail, session_dir, error
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                runs,
            )
    finally:
        conn.close()

    counts: dict[str, int] = {}
    for issue in issues:
        counts[issue["state"]] = counts.get(issue["state"], 0) + 1
    rejected = by_state.get("rejected")
    summary = {
        "db": str(db),
        "sessions": str(sessions),
        "issues": len(issues),
        "states": counts,
        "runners": len(RUNNERS),
        "runs": len(runs),
        "events": len(events),
        "rejected_fingerprint": rejected["fingerprint"] if rejected else None,
        "transcript": transcript,
    }
    return summary


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Seed a local Nuwa database for console E2E.")
    parser.add_argument("--db", required=True, help="SQLite database path (created/migrated if missing)")
    parser.add_argument("--bin", required=True, help="path to the nuwa binary (or a name on PATH)")
    parser.add_argument("--sessions", required=True, help="absolute session root for the transcript")
    parser.add_argument("--issues", type=int, default=8, help="number of issues to insert (default 8)")
    args = parser.parse_args(argv)
    if args.issues < 1:
        parser.error("--issues must be >= 1")

    db = Path(args.db).expanduser().resolve()
    sessions = Path(args.sessions).expanduser().resolve()
    db.parent.mkdir(parents=True, exist_ok=True)
    summary = seed(db, resolve_bin(args.bin), sessions, args.issues)

    print(f"rejected_fingerprint: {summary['rejected_fingerprint'] or '(none)'}")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
