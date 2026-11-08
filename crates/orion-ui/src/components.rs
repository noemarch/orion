//! Components module - placeholder for future UI components.

pub mod placeholder {
    use gpui::prelude::*;
    use gpui::{div, Context, Render, Window};

    pub struct PlaceholderComponent;

    impl PlaceholderComponent {
        pub fn new(_cx: &mut Context<Self>) -> Self {
            Self
        }
    }

    impl Render for PlaceholderComponent {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().child("Placeholder")
        }
    }
}
