## ADDED Requirements

### Requirement: Restore Pre-Dialog Mode

The TUI permission dialog SHALL snapshot the current `AppMode` when it
opens and SHALL restore exactly that mode on every decision path
(Allow / AllowAlways / Deny / Esc). It MUST NOT hard-code
`AppMode::Streaming` as the post-dialog mode.

#### Scenario: Opened during streaming
- **GIVEN** mode is `Streaming` and the dialog opens
- **WHEN** the user Denies
- **THEN** mode returns to `Streaming`

#### Scenario: Opened after streaming ended
- **GIVEN** the final assistant block has committed, mode is `Input`,
  and the dialog opens
- **WHEN** the user Denies
- **THEN** mode returns to `Input`, not `Streaming`

#### Scenario: Allow-Always path restores identically
- **GIVEN** the dialog opens during any mode
- **WHEN** the user selects Allow Always
- **THEN** the mode after the dialog matches the mode before the dialog
