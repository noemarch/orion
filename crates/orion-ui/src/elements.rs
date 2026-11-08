//! Elements module - placeholder for reusable UI elements.

pub mod placeholder {
    use gpui::prelude::*;
    use gpui::{div, Context, Render, Window};

    pub struct PlaceholderElement;

    impl PlaceholderElement {
        pub fn new(_cx: &mut Context<Self>) -> Self {
            Self
        }
    }

    impl Render for PlaceholderElement {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().child("Placeholder Element")
        }
    }
}
