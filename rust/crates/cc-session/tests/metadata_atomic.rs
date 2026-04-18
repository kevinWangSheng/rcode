//! Atomicity regression test for `Session::write_metadata`.
//!
//! Spec: `openspec/changes/fix-session-writeln-fsync/specs/session-persistence/spec.md`
//! Scenario "Metadata write is atomic":
//!
//! > GIVEN `write_metadata(meta)` is called and crashes mid-write
//! > WHEN the next process reads `metadata.json`
//! > THEN it sees either the previous fully-written version or no file,
//! >     never a truncated JSON document
//!
//! The prior implementation used `fs::write(&path, json)`, which opens
//! with `O_TRUNC` and writes in place: a crash between the truncate and
//! the final flush would leave an empty or partial `metadata.json` that
//! `load_metadata` cannot deserialize. The fix uses a same-directory
//! `NamedTempFile` + `persist` (atomic rename), so readers always see
//! either the prior full version or the new full version.
//!
//! The `concurrent_reader_never_sees_truncated_metadata` test below is
//! designed to fail against the old `fs::write` implementation (which
//! opens with `O_TRUNC` and leaves a zero-byte window observable by a
//! concurrent reader) and pass against the tmpfile+persist
//! implementation (which replaces the file via atomic rename).
//!
//! Tests serialize on a process-wide mutex to avoid racing each other's
//! tempdir cleanup, which we have observed to cause spurious ENOENTs
//! from `persist` on macOS.

use cc_session::{Session, SessionMetadata};
use std::path::PathBuf;

/// Process-wide lock so these filesystem-heavy tests don't race each
/// other (we had spurious `persist: ENOENT` failures from parallel
/// `TempDir::drop` vs. another test's `NamedTempFile::new_in`).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// A bare `Session` rooted at a caller-supplied transcript path, so the
/// test doesn't have to mutate `HOME` and race with other integration
/// tests in this binary.
///
/// `Session`'s fields are private outside the crate, so we reach it
/// through the public `resume` API after pre-seeding the transcript on
/// disk. That's both the realistic path (a session existing on disk) and
/// keeps the test black-box.
fn session_in(dir: &std::path::Path) -> Session {
    // Seed a transcript file so `resume` takes the happy Rust-layout path.
    let id = format!(
        "meta-atomic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let home = dir;
    let session_dir = home.join(".claude").join("sessions").join(&id);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("transcript.jsonl"), b"").unwrap();

    // Scope the HOME override to the resume call. Tests in this file
    // serialize on `SERIAL` so they don't race each other.
    let _guard = EnvLock::with_home(home);
    let (session, _) = Session::resume(&id).expect("resume seeded session");
    session
}

fn metadata_path(session: &Session) -> PathBuf {
    session
        .transcript_path()
        .parent()
        .expect("transcript has parent")
        .join("metadata.json")
}

/// Round-trip assertion: after every `write_metadata`, `load_metadata`
/// must return a fully-parseable, schema-correct `SessionMetadata`.
#[test]
fn write_metadata_two_writes_roundtrip_cleanly() {
    let _s = serial_guard();
    let tmp = make_tempdir();
    let session = session_in(tmp.path());

    let m1 = SessionMetadata {
        model: "claude-sonnet-4-6".into(),
        started_at: "2026-04-17T10:00:00Z".into(),
        project_path: Some("/proj/one".into()),
        cwd: Some("/proj/one".into()),
    };
    session.write_metadata(&m1).expect("first write_metadata");
    let loaded1 = session
        .load_metadata()
        .expect("load after first write")
        .expect("metadata present after first write_metadata");
    assert_eq!(loaded1.model, "claude-sonnet-4-6");
    assert_eq!(loaded1.project_path.as_deref(), Some("/proj/one"));

    // Second write must not leave a torn file even though it overwrites.
    let m2 = SessionMetadata {
        model: "claude-opus-4-7".into(),
        started_at: "2026-04-17T11:00:00Z".into(),
        project_path: Some("/proj/two".into()),
        cwd: None,
    };
    session.write_metadata(&m2).expect("second write_metadata");
    let loaded2 = session
        .load_metadata()
        .expect("load after second write")
        .expect("metadata present after second write_metadata");
    assert_eq!(loaded2.model, "claude-opus-4-7");
    assert_eq!(loaded2.project_path.as_deref(), Some("/proj/two"));
    assert!(loaded2.cwd.is_none());
}

/// Simulate the crash mode the spec cares about: a sibling tmpfile
/// has been written into the session directory but never `persist`ed
/// (the process died between `write_all` and `rename`). The existing
/// `metadata.json` MUST NOT be affected — no truncation, no corruption,
/// still the prior full version.
#[test]
fn crashed_sibling_tempfile_does_not_corrupt_metadata() {
    let _s = serial_guard();
    let tmp = make_tempdir();
    let session = session_in(tmp.path());
    let meta_path = metadata_path(&session);
    let dir = meta_path.parent().unwrap().to_path_buf();

    // Step 1: write a full, valid metadata.json.
    let original = SessionMetadata {
        model: "claude-opus-4-7".into(),
        started_at: "2026-04-17T12:00:00Z".into(),
        project_path: Some("/keep/me".into()),
        cwd: None,
    };
    session.write_metadata(&original).expect("seed metadata");
    let bytes_before = std::fs::read(&meta_path).expect("metadata after seed");

    // Step 2: simulate a crashed in-flight `write_metadata`: a
    // `NamedTempFile` gets created in the session dir and partially
    // filled, then dropped without `persist`. This is the exact state
    // the filesystem would observe if the process was SIGKILL'd between
    // the tmpfile write and the rename.
    {
        let tmpfile =
            tempfile::NamedTempFile::new_in(&dir).expect("sibling tempfile in session dir");
        use std::io::Write as _;
        let mut f = tmpfile.as_file();
        f.write_all(b"{\"model\":\"half-written")
            .expect("partial write");
        f.flush().ok();
        // Drop without persist — tempfile unlinks itself.
        drop(tmpfile);
    }

    // Step 3: `metadata.json` must still be the original, byte-for-byte.
    let bytes_after =
        std::fs::read(&meta_path).expect("metadata.json must still exist after sibling crash");
    assert_eq!(
        bytes_before, bytes_after,
        "metadata.json was mutated by an unrelated sibling tempfile"
    );

    let reloaded = session
        .load_metadata()
        .expect("reload after sibling crash")
        .expect("metadata.json still present after sibling crash");
    assert_eq!(reloaded.model, "claude-opus-4-7");
    assert_eq!(reloaded.project_path.as_deref(), Some("/keep/me"));
}

/// The core atomicity regression: a reader racing a `write_metadata`
/// writer must never observe a truncated or empty `metadata.json`.
///
/// `fs::write` opens the target with `O_TRUNC`, creating a window —
/// from the first `open()` syscall until the final `write()` flush —
/// during which the on-disk file is zero bytes. A concurrent reader
/// that opens `metadata.json` in that window sees empty bytes, which
/// `serde_json::from_str` rejects.
///
/// The tmpfile+persist pattern closes this window: the target file is
/// replaced by atomic rename, so every reader sees either the full old
/// version or the full new version — never an empty/partial one.
///
/// A large JSON payload (~64 KiB of padding in `project_path`) makes
/// the truncate/write window reliably observable in CI.
#[test]
fn concurrent_reader_never_sees_truncated_metadata() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let _s = serial_guard();
    let tmp = make_tempdir();
    let session = session_in(tmp.path());
    let meta_path = metadata_path(&session);

    let big_suffix = "x".repeat(64 * 1024);
    let seed_meta = SessionMetadata {
        model: "seed".into(),
        started_at: "2026-04-17T00:00:00Z".into(),
        project_path: Some(format!("/seed-{big_suffix}")),
        cwd: None,
    };
    session.write_metadata(&seed_meta).expect("seed write");

    let raw = std::fs::read_to_string(&meta_path).expect("seed readable");
    let _: serde_json::Value = serde_json::from_str(&raw).expect("seed valid JSON");

    let stop = Arc::new(AtomicBool::new(false));
    let saw_truncation = Arc::new(AtomicBool::new(false));

    let reader_path = meta_path.clone();
    let stop_r = Arc::clone(&stop);
    let saw_r = Arc::clone(&saw_truncation);
    let reader = std::thread::spawn(move || {
        while !stop_r.load(Ordering::Relaxed) {
            match std::fs::read_to_string(&reader_path) {
                Ok(bytes) if bytes.is_empty() => {
                    saw_r.store(true, Ordering::Relaxed);
                    return;
                }
                Ok(bytes) => {
                    if serde_json::from_str::<serde_json::Value>(&bytes).is_err() {
                        saw_r.store(true, Ordering::Relaxed);
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    });

    for i in 0..200 {
        let meta = SessionMetadata {
            model: format!("iter-{i}"),
            started_at: "2026-04-17T00:00:00Z".into(),
            project_path: Some(format!("/iter-{i}-{big_suffix}")),
            cwd: None,
        };
        session.write_metadata(&meta).expect("write_metadata");
    }

    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader thread join");

    assert!(
        !saw_truncation.load(Ordering::Relaxed),
        "concurrent reader observed an empty or unparseable metadata.json; \
         write_metadata is not atomic"
    );

    let final_meta = session
        .load_metadata()
        .expect("final load_metadata")
        .expect("final metadata.json present");
    assert_eq!(final_meta.model, "iter-199");
}

// --- env guard + tempdir helpers (mirrors durability.rs) -------------

struct EnvLock<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
    prev: Option<std::ffi::OsString>,
}

static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl<'a> EnvLock<'a> {
    fn with_home(new: &std::path::Path) -> Self {
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
    fn path(&self) -> &std::path::Path {
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
    base.push(format!(
        "cc-session-metadata-atomic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create tempdir");
    TempDir(base)
}
