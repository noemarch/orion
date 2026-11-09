# gpui-kit Spike

Date: 2026-11-10

## Attempt

Created a temporary spike crate depending on `gpui = "0.2"` and `gpui-kit = "0.1"`. The crates.io index resolved `gpui-kit 0.1.0`, but a full `cargo check` timed out while compiling shared Windows/image dependencies before producing rendered component evidence.

## Finding

gpui-kit resolves with the pinned GPUI 0.2 line, but this spike did not produce a rendered Button/Dialog proof within the allotted time. The failure is in dependency compilation coverage, not in Orion code.

## Recommendation

Defer gpui-kit adoption for #7. Use raw GPUI with the same component boundaries and Orbit token contract defined in the design spec. Revisit gpui-kit only if a later spike produces a rendered component on the pinned Windows toolchain.
