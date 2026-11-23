# Custom overlays share one stateless modal shell

Context: Orion's Settings, Add Workspace, and Shell Picker are custom GPUI overlays that need the same panel, header, close, and focus behavior, while two running-process confirmations are gpui-kit alert dialogs. Migrating every surface to gpui-kit's `Dialog`/`Sheet` family was considered, but the 0.9.1 behavior and focus contracts for those surfaces were not verified and the migration would change more than presentation.

Decision: custom overlays use one stateless `ModalShell` presentation helper; each modal view owns its open state, form state, focus handle, and opener focus. gpui-kit alert dialogs remain kit-owned and receive only verified token/copy changes. Settings remains the already-shipped visual reference and is not refactored in this pass.

Considered Options:

- Inline the shell layout in each modal; rejected because the same focus and dismissal logic would drift.
- Migrate all five surfaces to gpui-kit `Dialog`/`Sheet`; rejected because it carries unverified API and behavior risk beyond a styling pass.
- Shared stateless shell; accepted because it standardizes presentation without moving state ownership.

Consequences: a future custom modal should reuse `ModalShell` rather than hand-rolling a scrim and panel; changes to focus trapping or dismissal happen once. The shell must stay generic and must not import gpui-kit `Settings` types.
