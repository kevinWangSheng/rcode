## 1. State

- [ ] 1.1 Add `pre_permission_mode: Option<AppMode>` on `App`.
- [ ] 1.2 Populate in the `ShowPermission` action handler.

## 2. Decision Arms

- [ ] 2.1 `PermissionAllow`, `PermissionAllowAlways`, `PermissionDeny`,
      and any Esc/Cancel path all restore from the snapshot.

## 3. Tests

- [ ] 3.1 Test: dialog opens during `Streaming`, Deny returns to
      `Streaming`.
- [ ] 3.2 Test: dialog opens after streaming ended (mode was `Input`);
      Deny returns to `Input`, not `Streaming`.

## 4. Sign-off

- [ ] 4.1 `cargo test -p cc-tui` + clippy clean.
