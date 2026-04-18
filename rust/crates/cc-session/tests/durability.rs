//! Transcript durability tests for `cc-session`.
//!
//! Verifies that every `Session::append` is fsynced before it returns,
//! so a SIGKILL / power loss after the call cannot truncate the JSONL
//! tail. See `openspec/changes/fix-session-writeln-fsync/proposal.md`.
//!
//! Strategy (task 3.2's `unsafe { libc::_exit(9) }` variant): we spawn
//! a fresh subprocess — a re-exec of this test binary, filtered to the
//! same test name via `--exact`, but with a cookie env var set — that
//! performs two appends and then calls `libc::_exit(9)` so nothing in
//! libc/Rust gets a chance to flush buffers or run destructors. This
//! mirrors SIGKILL / OOM / power-loss semantics closely enough to
//! detect a missing `sync_all`.

#![cfg(unix)]

use cc_core::MessageParam;
use cc_session::Session;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Parent -> child: the tempdir to use as HOME. Presence of this var in
/// the environment switches the test into child (crash) mode.
const CHILD_HOME_ENV: &str = "CC_SESSION_DURABILITY_CHILD_HOME";
/// Child -> parent: file to which the child writes its generated
/// session id just before appending + _exit.
const CHILD_ID_FILE_ENV: &str = "CC_SESSION_DURABILITY_CHILD_ID_FILE";

#[test]
fn transcript_survives_abrupt_child_exit() {
    // Child mode: do the crash-simulation work, never return.
    if let (Ok(home), Ok(id_file)) = (
        std::env::var(CHILD_HOME_ENV),
        std::env::var(CHILD_ID_FILE_ENV),
    ) {
        child_body(&home, Path::new(&id_file));
    }

    // Parent mode: orchestrate.
    let tmp_home = make_tempdir();
    let id_file = tmp_home.path().join("session-id.txt");

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("transcript_survives_abrupt_child_exit")
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_HOME_ENV, tmp_home.path())
        .env(CHILD_ID_FILE_ENV, &id_file)
        .status()
        .expect("spawn crash child");
    assert_eq!(
        status.code(),
        Some(9),
        "child must terminate via _exit(9); got {status:?}"
    );

    let session_id = std::fs::read_to_string(&id_file)
        .expect("child must have written its session id before _exit")
        .trim()
        .to_string();
    assert!(!session_id.is_empty(), "child recorded an empty session id");

    // Reopen the transcript in the parent. Both messages must be present
    // because `append` fsynced each one before the child _exited.
    // HOME is scoped via a single test-wide lock to avoid racing the
    // other integration tests in this binary.
    let _env = EnvLock::with_home(tmp_home.path());
    let (_session, messages) = Session::resume(&session_id).expect("parent resume must succeed");
    let transcript = transcript_path_for(tmp_home.path(), &session_id);
    assert_eq!(
        messages.len(),
        2,
        "both fsynced messages must survive _exit(9); transcript=\n{}",
        std::fs::read_to_string(&transcript).unwrap_or_default()
    );
}

/// Child body: create a session, record its id, append two messages,
/// then _exit(9) without cleanup. Never returns.
fn child_body(home: &str, id_file: &Path) -> ! {
    // SAFETY: fresh subprocess, no test-harness threads yet.
    unsafe {
        std::env::set_var("HOME", home);
    }

    let session = Session::new().expect("child Session::new");
    std::fs::write(id_file, &session.id).expect("child id_file write");

    let m1 = MessageParam::user("first message");
    let m2 = MessageParam::assistant("second message");
    session.append(&m1).expect("child append #1");
    session.append(&m2).expect("child append #2");

    // Bypass all Rust/libc shutdown: simulate SIGKILL / OOM.
    // SAFETY: _exit is async-signal-safe and does not return.
    unsafe { libc::_exit(9) };
}

fn transcript_path_for(home: &Path, id: &str) -> PathBuf {
    home.join(".claude")
        .join("sessions")
        .join(id)
        .join("transcript.jsonl")
}

// --- env guard + tempdir helpers (avoid adding new dev-deps) ---------

/// Process-wide lock so env-var mutation in the parent does not race
/// other tests in the same integration-test binary.
struct EnvLock<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
    prev: Option<std::ffi::OsString>,
}

static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl<'a> EnvLock<'a> {
    fn with_home(new: &Path) -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("HOME");
        // SAFETY: holding the mutex serializes env mutation across this
        // binary's threads.
        unsafe {
            std::env::set_var("HOME", new);
        }
        EnvLock {
            _guard: guard,
            prev,
        }
    }
}

impl<'a> Drop for EnvLock<'a> {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn make_tempdir() -> TempDir {
    let mut base = std::env::temp_dir();
    base.push(format!("cc-session-durability-{}", uuid_like()));
    std::fs::create_dir_all(&base).expect("create tempdir");
    TempDir(base)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), t)
}
