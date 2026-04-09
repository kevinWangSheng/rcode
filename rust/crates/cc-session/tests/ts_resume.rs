//! End-to-end test for resuming a session that was originally written by the
//! TypeScript Claude Code (M4 exit criterion 8).
//!
//! Drops a TS-format JSONL transcript under a fake `$HOME/.claude/projects/<slug>/<id>.jsonl`
//! and asserts that `Session::resume(<id>)` finds it and parses the messages.

use std::fs;

use cc_core::{MessageContent, Role};
use cc_session::Session;
use tempfile::tempdir;

#[test]
fn resumes_ts_format_session_via_projects_layout() {
    let dir = tempdir().unwrap();

    // Point HOME at a fresh temp dir so dirs::home_dir() returns it.
    // SAFETY: env mutation in a single-test file. The crate has tests that
    // also run, but cargo runs `tests/*.rs` integration tests in separate
    // processes, so this only affects this test binary.
    // SAFETY: setting an env var is safe in single-threaded test context.
    unsafe {
        std::env::set_var("HOME", dir.path());
    }

    let project_slug = "-Users-test-some-project";
    let session_id = "11111111-2222-3333-4444-555555555555";
    let session_path = dir
        .path()
        .join(".claude")
        .join("projects")
        .join(project_slug);
    fs::create_dir_all(&session_path).unwrap();
    let jsonl_path = session_path.join(format!("{session_id}.jsonl"));

    // Two real turns + one summary line that should be skipped.
    let lines = [
        r#"{"type":"summary","text":"prior session"}"#,
        r#"{"type":"user","message":{"role":"user","content":"hello from TS"}}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi from TS"}]}}"#,
    ];
    fs::write(&jsonl_path, lines.join("\n")).unwrap();

    let (session, messages) = Session::resume(session_id).expect("resume should find TS session");
    assert_eq!(session.id, session_id);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, Role::User);
    match &messages[0].content {
        MessageContent::Text(t) => assert_eq!(t, "hello from TS"),
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(messages[1].role, Role::Assistant);
    let MessageContent::Blocks(blocks) = &messages[1].content else {
        panic!("expected blocks for assistant turn");
    };
    assert_eq!(blocks.len(), 1);
}
