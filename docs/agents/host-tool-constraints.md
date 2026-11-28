# Host tool limits

Four limits of this workstation, each rediscovered by every agent that runs here. Each heading is one workaround; the target is what to do, the failure is what it prevents.

## LSP daemon times out

`lsp_symbols` and friends time out after 30 seconds on this host. Retry once, then fall back to a full file read or `grep` for the symbol inventory. The failure it prevents: a 30-second stall per lookup, repeated for every symbol, with no result at the end.

## Tee-Object buffers cargo output

Redirect cargo output straight to a file with `*>` (for example `cargo test -p orion-ui *> .omo/evidence/run.txt`). Do not pipe a recompile through `Tee-Object`. The failure it prevents: PowerShell buffering the stream while rustc runs, ending in `rustc-LLVM out of memory` / `STATUS_STACK_BUFFER_OVERRUN`.

## A stale orion.exe daemon locks the build

Stop any stray `orion.exe` (or `orion-ui.exe`) process before rebuilding, and confirm no daemon from an earlier run is still alive. The failure it prevents: `cargo` refusing to write `G:\Temp\rust\target\debug\orion.exe` because a previous daemon holds it open, which blocks the whole build.

## Cargo memory on a constrained host

Set `CARGO_BUILD_JOBS=1` to avoid Windows commit-limit errors, and `CARGO_PROFILE_TEST_DEBUG=0` to lower codegen memory when the host OOMs during a test build. These are local-run differences only: CI runs the same gates with its own defaults, so they never replace the CI commands.
