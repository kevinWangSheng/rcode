# fix-file-history-backup-versioning

Follow-up to `fix-file-history-snapshot-producers`. The shipped
producer always writes its sidecar at `{hash}@v1` and
`tmp.persist(&backup_path)` overwrites; two successful Edits of the
same path in one session therefore clobber the first Edit's pre-
image, even though both snapshot entries remain in the JSONL.
Replay/rewind to the first snapshot then restores the **second**
Edit's pre-image bytes — silent data corruption.

This change makes the sidecar name carry a per-path version so
every snapshot entry points at a distinct backup file. TS has the
same `{hash}@v{n}` naming; the guard it uses to avoid the collision
(`fileHistoryTrackEdit` bails when the path is already tracked in
the current turn) is deferred on the Rust side along with per-turn
tracking (`is_snapshot_update` §5 of the parent change), so the
fix here is version-increment rather than skip-if-tracked.

**Depends on**: `fix-file-history-snapshot-producers` (merged in
`cd3b9b5`). No upstream blockers.
