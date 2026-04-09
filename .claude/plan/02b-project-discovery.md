# Phase 1 Gap Patch A3 — Project Discovery, `.claude/` Rules, CLAUDE.md Loading

**Status:** COMPLETE (2026-04-09)
**Depends on:** Phase 1 outputs (01–08)
**Blocks:** Decisions 1–4, Phase 2 detailed design

---

## 1. Project Root Resolution

### Algorithm: Git Walk-Up

**TS Source:** `src/utils/git.ts:27-109`

1. Start from `startPath` (normalized to NFC Unicode form)
2. Walk upward via `dirname()` toward filesystem root
3. At each directory, check for `.git` (file or directory)
4. `.git` as **directory** = normal repository
5. `.git` as **file** = worktree or submodule (contains `gitdir: <path>` reference)
6. Return the directory containing `.git`, or `null` if none found

**Caching:** LRU cache (max 50 entries), keyed by `startPath`. Cleared manually via `.cache.clear()`.

**Diagnostic logging:** `find_git_root_started` / `find_git_root_completed` with `duration_ms`, `stat_count`, `found`.

### Canonical Root Resolution

**TS Source:** `src/utils/git.ts:111-180`

For worktrees, resolves through the chain to find the **main repository root**:

1. Read `.git` file → extract `gitdir:` path
2. Read `<gitdir>/../commondir` (or `<gitdir>/commondir`) to find shared `.git` directory
3. Resolve to main repo working directory

**Security validations (anti-symlink-attack):**
1. Validate `worktreeGitDir` is a direct child of `<commonDir>/worktrees/`
2. Validate back-link: `<worktreeGitDir>/gitdir` points back to `<gitRoot>/.git`
3. Use `realpathSync()` on directories to resolve symlinks before comparison
4. Fall through to input root on validation failure (defensive)

**Bare repository handling:** If `basename(commondir) !== '.git'`, return commondir itself.

**Purpose:** Project-scoped state (auto-memory, project config) is shared across all worktrees of the same repo.

### Fallback (No Git Repo)

If no `.git` found: `getProjectPathForConfig()` returns `resolve(getOriginalCwd())`.

**TS Source:** `src/utils/config.ts:1588-1600`

```
getProjectPathForConfig = memoize(() => {
  gitRoot = findCanonicalGitRoot(getOriginalCwd())
  return normalizePathForConfigKey(gitRoot ?? resolve(getOriginalCwd()))
})
```

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| Walk-up from CWD to find `.git` | MUST REPLICATE EXACTLY |
| Canonical root via worktree chain | MUST REPLICATE EXACTLY |
| Security validation (symlink, back-link) | MUST REPLICATE EXACTLY |
| LRU caching (max 50) | SHOULD REPLICATE (performance, not correctness) |
| Fallback to `resolve(cwd)` when not in git | MUST REPLICATE EXACTLY |
| Bare repo detection | MUST REPLICATE EXACTLY |
| NFC normalization of paths | MUST REPLICATE EXACTLY (macOS HFS+ decomposition) |

---

## 2. `.claude/` Directory Structure

### Expected Layout

```
~/.claude/                              # User global (CLAUDE_CONFIG_DIR override)
├── settings.json                       # User settings
├── cowork_settings.json                # Cowork mode settings (mutually exclusive)
├── CLAUDE.md                           # User memory file
├── rules/                              # User conditional rules
│   └── *.md                            # Rule files with frontmatter globs
├── keybindings.json                    # Custom keybindings
├── memory/                             # Auto-memory directory
│   └── memory.md                       # Auto-managed memory
└── teams/                              # Team configuration

<project-root>/.claude/                 # Project-scoped
├── settings.json                       # Project settings (checked in)
├── settings.local.json                 # Local settings (gitignored)
├── CLAUDE.md                           # Project memory (checked in)
├── rules/                              # Project conditional rules
│   └── *.md                            # Rule files with frontmatter globs
└── memory/                             # Project memory state

<managed-path>/                         # Policy / managed settings
├── managed-settings.json               # Base managed settings
├── managed-settings.d/                 # Drop-in directory
│   └── *.json                          # Alphabetically sorted, later wins
├── CLAUDE.md                           # Managed memory file
└── .claude/
    └── rules/                          # Managed rules
        └── *.md
```

**Managed path by platform:**
- macOS: `/Library/Application Support/ClaudeCode/`
- Linux: `/etc/claude-code/`
- Windows: `C:\Program Files\ClaudeCode\`

### Environment Variables

| Variable | Effect | Default |
|----------|--------|---------|
| `CLAUDE_CONFIG_DIR` | Override `~/.claude` path | `$HOME/.claude` |
| `CLAUDE_CODE_USE_COWORK_PLUGINS` | Use `cowork_settings.json` instead of `settings.json` | `false` |
| `CLAUDE_CODE_MANAGED_SETTINGS_PATH` | Override managed settings directory | Platform-specific |

---

## 3. Settings Merging

### Merge Order (lowest → highest priority)

**TS Source:** `src/utils/settings/settings.ts:673-784`

1. **Plugin Settings Base** (allowlisted keys only)
2. **User Settings** (`~/.claude/settings.json` or `cowork_settings.json`)
3. **Project Settings** (`.claude/settings.json`)
4. **Local Settings** (`.claude/settings.local.json`)
5. **Flag Settings** (`--settings <path>` CLI argument or inline JSON via SDK)
6. **Policy Settings** (managed — "first source wins" among sub-sources)

### Policy Settings Sub-Source Priority (highest → lowest)

1. **Remote** — synced from API (highest)
2. **MDM** — HKLM (Windows) or plist (macOS)
3. **File-based** — `managed-settings.json` + `managed-settings.d/*.json` (alphabetically sorted, later overrides)
4. **HKCU** — Windows user registry (lowest)

Once a source has content, earlier sources are not consulted ("first source wins").

### Merge Semantics

**TS Source:** `src/utils/settings/settings.ts:538-547`

| Type | Behavior |
|------|----------|
| Arrays | Concatenated and deduplicated (union via `uniq()`) |
| Objects | Deep merged recursively |
| Primitives | Later value overrides |
| `undefined` | Deletes key from merged object |

### Setting Source Enablement

Sources can be disabled via SDK or CLI:

- `isSettingSourceEnabled('projectSettings')` gates `.claude/settings.json`
- `isSettingSourceEnabled('localSettings')` gates `.claude/settings.local.json`
- `isSettingSourceEnabled('userSettings')` gates `~/.claude/settings.json`
- Control: `CLAUDE_CODE_SETTING_SOURCES` env var (comma-separated)

### Security: Dangerous Settings Exclusion

**TS Source:** `src/utils/settings/settings.ts:882-911`

The following settings are **excluded from `projectSettings`** to prevent RCE via malicious `.claude/settings.json`:

- `skipDangerousModePermissionPrompt`
- `skipAutoPermissionPrompt`
- `useAutoModeDuringPlan`
- `autoMode` (classifier rules)

These are only honored from: `userSettings`, `localSettings`, `flagSettings`, `policySettings`.

### Caching

- Settings cached at session level via `getSettingsWithErrors()`
- Invalidated via `resetSettingsCache()`
- No runtime hot-reload; changes require Claude Code restart

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| 6-layer merge order | MUST REPLICATE EXACTLY |
| Policy "first source wins" | MUST REPLICATE EXACTLY |
| Array union, deep object merge, undefined deletion | MUST REPLICATE EXACTLY |
| Dangerous settings exclusion from projectSettings | MUST REPLICATE EXACTLY (security) |
| Drop-in directory alphabetical sort | MUST REPLICATE EXACTLY |
| Setting source enablement | MUST REPLICATE EXACTLY |
| Session-level caching | SHOULD REPLICATE (performance) |

---

## 4. CLAUDE.md Loading

### Memory Types

**TS Source:** `src/utils/claudemd.ts`, `src/utils/memory/types.ts`

```
MemoryType = 'Managed' | 'User' | 'Project' | 'Local' | 'AutoMem' | 'TeamMem'
```

### Loading Order (lowest → highest priority)

1. **Managed** (`<managed-path>/CLAUDE.md` + `<managed-path>/.claude/rules/*.md`)
   - Always loaded (policy)

2. **User** (`~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`)
   - Only if `isSettingSourceEnabled('userSettings')`
   - External includes always allowed

3. **Project** (walk-up from CWD to root)
   - Only if `isSettingSourceEnabled('projectSettings')`
   - At each directory (ascending): `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/*.md`
   - External includes gated by approval config

4. **Local** (walk-up from CWD to root)
   - Only if `isSettingSourceEnabled('localSettings')`
   - At each directory: `CLAUDE.local.md`
   - Automatically added to `.gitignore`

5. **AutoMem** (`~/.claude/memory/memory.md` or configured path)
   - Only if auto-memory feature enabled

6. **TeamMem** (team-specific, synced)
   - Only if team memory feature enabled

### Walk-Up Algorithm

**TS Source:** `src/utils/claudemd.ts:790-1074`

```
1. dirs = []
2. currentDir = getOriginalCwd()
3. while currentDir != root:
     dirs.push(currentDir)
     currentDir = dirname(currentDir)
4. Process dirs in REVERSE order (root → CWD)
   For each dir:
     - Read CLAUDE.md           (type: Project)
     - Read .claude/CLAUDE.md   (type: Project)
     - Read .claude/rules/*.md  (type: Project, with frontmatter globs)
     - Read CLAUDE.local.md     (type: Local)
```

**Key behavior:** Files closer to CWD are loaded later → higher priority (model pays more attention to later context).

### Worktree Special Handling

**TS Source:** `src/utils/claudemd.ts:859-884`

When running from a worktree nested inside its main repo:

1. Detect: `findGitRoot(cwd)` differs from `findCanonicalGitRoot(cwd)`
2. If gitRoot is inside canonicalRoot → nested worktree
3. **Skip Project-type files** from directories above worktree but within main repo
4. Local memory (`CLAUDE.local.md`) is still loaded from main repo directories

**Rationale:** Worktree has its own checkout of `CLAUDE.md`, so loading from main repo would duplicate.

### External Include Support (@-directive)

**Syntax in memory files:**
- `@path` — treated as relative to file's directory
- `@./relative/path`
- `@~/home/path`
- `@/absolute/path`

**Rules:**
- Only in leaf text nodes (not in code blocks)
- Supported extensions: `.md`, `.txt`, `.json`, `.yaml`, `.js`, `.ts`, `.py`, `.go`, `.rs`, etc.
- Circular reference prevention via `processedPaths` tracking
- Non-existent files silently ignored
- Included files added as separate entries before the including file

**Security gating:**
- User memory: external includes always allowed
- Project memory: requires `config.hasClaudeMdExternalIncludesApproved`
- Managed memory: depends on approval config

### Additional Directories (--add-dir)

Controlled by `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` env var.
When enabled: loads `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/*.md` from each `--add-dir` path.

### Rules Files Frontmatter

`.claude/rules/*.md` files can have YAML frontmatter with glob patterns:

```yaml
---
globs: "*.rs"
---
Rule content here...
```

These rules only apply when working with files matching the glob pattern.

### MemoryFileInfo Structure

```typescript
{
  path: string,                    // Absolute file path
  type: MemoryType,                // Managed | User | Project | Local | AutoMem | TeamMem
  content: string,                 // File content (after include resolution)
  parent?: string,                 // Path of file that @-included this one
  globs?: string[],                // Frontmatter glob patterns (rules files)
  contentDiffersFromDisk?: boolean, // Content modified from disk version
  rawContent?: string,             // Original disk content before processing
}
```

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| Walk-up from CWD, root→CWD ordering | MUST REPLICATE EXACTLY |
| 6 memory types with loading order | MUST REPLICATE EXACTLY |
| Worktree duplicate-skip logic | MUST REPLICATE EXACTLY |
| @-include resolution with circular prevention | MUST REPLICATE EXACTLY |
| External include security gating | MUST REPLICATE EXACTLY |
| Rules frontmatter glob support | MUST REPLICATE EXACTLY |
| Auto-gitignore of CLAUDE.local.md | MUST REPLICATE EXACTLY |
| Setting source enablement gates | MUST REPLICATE EXACTLY |

---

## 5. `.claudeignore` — Does Not Exist

There is **no `.claudeignore` file** in the TS codebase. File exclusion uses:

1. **`.gitignore`** — standard git ignore rules, checked via `git check-ignore`
2. **Rules frontmatter globs** — `.claude/rules/*.md` can target specific file patterns (inclusion, not exclusion)

### Rust Contract

Do not implement `.claudeignore`. Use `.gitignore` integration for file exclusion.

---

## 6. Startup and Initialization Order

### Initialization Sequence

1. **CWD Capture:** `setOriginalCwd(process.cwd())` — captured once at startup, immutable
2. **Git Root Resolution:** `findGitRoot()` + `findCanonicalGitRoot()` — cached
3. **Global Config Load:** `getGlobalConfig()` from `~/.claude/<config>.json`
4. **Project Config Lookup:** `getCurrentProjectConfig()` keyed by canonical root
5. **Settings Merge:** `getSettingsWithErrors()` — all 6 sources merged, cached
6. **Memory Files Load:** `getMemoryFiles()` — all CLAUDE.md files discovered, cached
7. **Trust Check:** `checkHasTrustDialogAccepted()` — project-scoped trust state

### Project Identity

- Keyed by `normalizePathForConfigKey(canonicalGitRoot ?? resolve(cwd))`
- Stored in `~/.claude/<config>.json` → `projects[<normalized-path>]`
- Used for: trust state, project-specific config, session tracking

---

## 7. Environment Variables Summary

| Variable | Component | Effect |
|----------|-----------|--------|
| `CLAUDE_CONFIG_DIR` | Config | Override `~/.claude` path |
| `CLAUDE_CODE_USE_COWORK_PLUGINS` | Settings | Toggle cowork mode settings file |
| `CLAUDE_CODE_MANAGED_SETTINGS_PATH` | Settings | Override managed settings path |
| `CLAUDE_CODE_SETTING_SOURCES` | Settings | Restrict which sources are loaded |
| `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` | Memory | Enable --add-dir CLAUDE.md loading |
| `CLAUDE_CODE_AUTO_COMPACT_WINDOW` | Compaction | Cap context window size |
