# Proposal — Add `-c` / `-r` short aliases; survey the rest

## Why

`fix-cli-print-short-flag` (commit `c197fb5`) landed `-p` for
`--print` but its open-question §1 explicitly deferred the rest of
the short-alias sweep. Running the full audit now — before a batch of
users / CI scripts pick up habits around the post-`-p`-only Rust CLI
— keeps the short-flag surface aligned to TS while the divergence
set is small.

### TS shorts on `program` (from `src/main.tsx:971,3808,3811`)

| TS short | TS long          | Rust long       | Rust short today | Status                 |
|----------|------------------|-----------------|------------------|------------------------|
| `-h`     | `--help`         | `--help`        | `-h`             | ✅ matches (clap auto)  |
| `-V`     | `--version`      | `--version`     | `-V`             | ✅ matches (clap auto)  |
| `-v`     | `--version`      | `--verbose`     | `-v`             | ⚠️ **CONFLICT** — see §3 |
| `-p`     | `--print`        | `--print`       | `-p`             | ✅ matches (`c197fb5`)  |
| `-c`     | `--continue`     | `--continue`    | —                | ❌ missing — THIS CHANGE |
| `-r`     | `--resume`       | `--resume`      | —                | ❌ missing — THIS CHANGE |
| `-d`     | `--debug`        | *(n/a)*         | —                | N/A — Rust has no `--debug` |
| `-n`     | `--name`         | *(n/a)*         | —                | N/A — Rust has no `--name`  |
| `-w`     | `--worktree`     | *(n/a)*         | —                | N/A — Rust has no `--worktree` |

Also appearing on TS subcommands (not `program`): `-s, --scope`, `-l,
--list`, `-a, --all`, `-d, --description`. All subcommand-scoped; no
cross-surface collision with the top-level flags. Rust doesn't expose
those subcommands (`mcp`, `plugin`, `task`) yet, so nothing to align.

## Goal

Make `claude -c` and `claude -r <id>` behave identically to their
long forms. Every short alias TS has AND Rust already has the long
form for MUST now work on the Rust side too.

## What changes

### 1. `rust/cc/src/main.rs` — two attribute tweaks

```rust
// Continue (bool)
-    /// Resume the most recent session (mutually exclusive with --resume).
-    #[arg(long)]
-    r#continue: bool,
+    /// Resume the most recent session (mutually exclusive with --resume).
+    #[arg(short = 'c', long)]
+    r#continue: bool,

// Resume (optional value)
-    /// Resume a previous session by ID.
-    #[arg(long, value_name = "SESSION_ID")]
-    resume: Option<String>,
+    /// Resume a previous session by ID.
+    #[arg(short = 'r', long, value_name = "SESSION_ID")]
+    resume: Option<String>,
```

Both letters are free in the current Rust CLI (existing shorts are
`-m` / `-p` / `-v`). `-r` is intentionally placed where TS has it
even though TS's `--resume` accepts an optional value
(`[value]` → bare `-r` opens an interactive picker) while Rust's
currently requires a value; the value-optional divergence is a
separate spec gap, tracked in §3.

### 2. Tests

Add two clap-level alias-equivalence tests in `cc/src/main.rs::tests`
alongside `print_short_flag_p_equivalent_to_long`:

```rust
#[test]
fn continue_short_flag_c_equivalent_to_long() {
    use clap::Parser;
    let long = Cli::parse_from(["claude", "--continue"]);
    let short = Cli::parse_from(["claude", "-c"]);
    assert!(long.r#continue);
    assert!(short.r#continue);
}

#[test]
fn resume_short_flag_r_equivalent_to_long() {
    use clap::Parser;
    let long = Cli::parse_from(["claude", "--resume", "sess-abc"]);
    let short = Cli::parse_from(["claude", "-r", "sess-abc"]);
    assert_eq!(long.resume.as_deref(), Some("sess-abc"));
    assert_eq!(short.resume.as_deref(), Some("sess-abc"));
    assert_eq!(long.resume, short.resume);
}
```

### 3. Divergences held out of scope (documented, not fixed)

**`-v` meaning**. TS uses `-v` for `--version`, Rust uses `-v` for
`--verbose`. Changing this is user-visible: `claude -v` output flips
from "nothing visible + verbose logs later" to "version string, exit
0". That's a breaking UX decision (people's `.zshrc` aliases, CI
scripts asking for version, etc.) and isn't a "short alias" fix —
it's a meaning change. File a separate change if the team decides
to align; until then, Rust keeps `-v = --verbose` and users get
`--version` via `-V` (clap default) or the full long form.

**`--resume [optional-value]`**. TS lets bare `-r` / `--resume` open
an interactive session picker; Rust's `--resume` (both long and new
short) requires a SESSION_ID. This is a behavioural gap, not an
alias gap — track separately as `fix-cli-resume-bare-picker` if the
picker UX lands.

**Flags Rust doesn't implement yet**. `--debug` (with `-d`), `--name`
(with `-n`), `--worktree` (with `-w`) are all on TS's program but
absent from Rust. When each of those flags lands, the corresponding
short MUST be declared at the same time; no need to pre-book them
here.

## Impact

- **Affected specs**: `cli-flags` (MODIFIED — adds two new short-alias
  scenarios to the cap introduced by `fix-cli-print-short-flag`).
- **Affected crates**: `cc` (claude binary) only.
- **Wire / behaviour compatibility**: strictly additive. `-c` and
  `-r` previously errored with `unexpected argument`; now they work.
  Long forms unchanged. No existing shorts touched.

## Open questions

1. Should Rust gain a value-optional `--resume` to match TS's
   interactive picker behaviour? Out of scope here — the picker UI
   component would need to exist first. File separately when the
   TUI/headless picker lands.
2. Is the `-v` divergence worth flipping? Needs product sign-off; not
   this change.
