//! The harness Orion's UI tests run on.
//!
//! The kit ships `#[gpui_kit::test]`, but its expansion is deeper than
//! stable rustc can hold: even a two-test crate that uses nothing but the
//! attribute fails with "recursion limit reached while expanding `#[test]`",
//! and raising the limit only trades that error for a STATUS_STACK_OVERFLOW.
//! The kit's own test API is public, so this harness drives it directly —
//! one dispatcher, one app context, and the same teardown the attribute
//! performs — and [`orion_test`] takes the attribute's place.
//!
//! The body still receives a `&mut TestAppContext`, so a test reads the
//! same as it did under the kit's attribute:
//!
//! ```ignore
//! orion_test!(fn the_button_answers(cx: &mut gpui_kit::TestAppContext) {
//!     cx.run_until_parked();
//! });
//! ```

use gpui_kit::{TestAppContext, TestDispatcher};

// Run one UI test body on a fresh GPUI test context.
//
// The context is built, the body runs, the context is drained and quit, and
// the dispatcher's tasks are dropped — the same sequence the kit's test
// attribute performs around a test body.
pub fn run_ui_test(name: &'static str, body: impl FnOnce(&mut TestAppContext)) {
    let dispatcher = TestDispatcher::new(0);
    let mut cx = TestAppContext::build(dispatcher.clone(), Some(name));
    let _entity_refcounts = cx.app.borrow().ref_counts_drop_handle();

    // Kit components paint from the kit's global theme, and the application
    // installs it through `gpui_kit::init`. A test context has no application
    // behind it, so it installs the same theme here — Quiet Slate included,
    // through the seam the app uses — before the body renders a control.
    cx.update(|app| {
        gpui_kit::init(app);
        crate::theme::sync_gpui_kit(app);
    });

    body(&mut cx);

    cx.run_until_parked();
    cx.update(|cx| {
        cx.background_executor().forbid_parking();
        cx.quit();
    });
    cx.run_until_parked();
    drop(cx);
    dispatcher.drain_tasks();
}

// A test that runs its body on a GPUI test context.
//
// It replaces `#[gpui_kit::test]`, whose expansion this toolchain cannot
// compile (see the module docs). It is called in bang position — Rust only
// accepts macro_rules macros in attribute position when they are
// attribute-like — and carries the test function over unchanged:
//
// ```ignore
// orion_test!(fn the_button_answers(cx: &mut gpui_kit::TestAppContext) {
//     cx.run_until_parked();
// });
// ```
#[macro_export]
macro_rules! orion_test {
    (fn $name:ident($cx:ident: &mut $ty:ty $(,)?) $body:block) => {
        #[::core::prelude::v1::test]
        fn $name() {
            $crate::test_support::run_ui_test(stringify!($name), |$cx: &mut $ty| $body)
        }
    };
}
