# gpui-kit Spike

Date: 2026-11-10 (updated)

## Attempt

Created a temporary spike crate depending on `gpui-kit = { version = "0.9.0", features = ["gpui-fast"] }` and rendered a styled component:

```rust
use gpui_kit::*;
use gpui_kit::component::button::{Button, ButtonVariants};

struct Hello;

impl Render for Hello {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(Button::new("hello").label("Hello").primary())
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Hello))
            .expect("failed to open window");
    });
}
```

`cargo check` passes on Windows with the pinned toolchain.

## Finding

`gpui-kit 0.9.1` (with `gpui-fast`) builds cleanly and provides `component::button::Button` through the `gpui_kit` facade. The earlier `gpui-kit 0.1.0` crate is a placeholder binary (`Hello, world!`) and must not be used.

## Recommendation

Adopt gpui-kit 0.9 with `gpui-fast` for shell chrome (Button, Tabs, Sidebar, Dialog, Badge) in a follow-up migration. Keep the custom VT grid and Orbit token contract. The migration must replace the raw `gpui` dependency in `crates/orion-ui/Cargo.toml` and verify the full workspace build, clippy, and screenshots.
