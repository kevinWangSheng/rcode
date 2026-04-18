## ADDED Requirements

### Requirement: Bounded Git Subprocess Calls

All `cc-git` subprocess invocations SHALL run under a bounded timeout
(default 5 s, overridable via `CC_GIT_IGNORE_TIMEOUT_MS`). On elapse
the child MUST be killed and the call SHALL return a safe default
(empty result / `false` flag) with a `warn!` log entry.

A hung or slow git call MUST NOT block CLI startup.

#### Scenario: Normal invocation is unchanged
- **GIVEN** a healthy repo with a few thousand files
- **WHEN** `filter_git_ignored` is called
- **THEN** the call returns well under 1 s with correct results

#### Scenario: Slow git degrades gracefully
- **GIVEN** a `git` invocation that sleeps 30 s
- **WHEN** the tool is called with the default timeout
- **THEN** the call returns within ~6 s with `vec![false; N]`
- **AND** a WARN log entry names the timeout and the path count
