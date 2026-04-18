## ADDED Requirements

### Requirement: Explicit Kill and Reap on Bash Cancel

The Bash tool SHALL explicitly `kill()` and `wait()` on the child
process before returning from a cancelled run. It MUST NOT rely solely
on the implicit drop-based kill, because the drop schedules a kill
that may not complete before the next bash call runs.

The same pattern SHALL apply to the timeout branch.

#### Scenario: Cancelled sleep is gone by return
- **GIVEN** a running `bash -c 'sleep 30'`
- **WHEN** the cancellation token fires
- **THEN** by the time the Bash tool returns, the child PID is no
  longer running
- **AND** no zombie is left for the system reaper
