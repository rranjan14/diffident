//! Orchestrator. Opens the window and wires crates together — no domain logic,
//! no rendering, no transport. If this file grows past wiring, the logic belongs
//! in a crate under `crates/`.

use diffident_model::Review;
use diffident_ui::Workspace;
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Workspace::new(placeholder_reviews())),
        )
        .unwrap();
        cx.activate(true);
    });
}

/// Stand-in until the forge crate lands and the rail loads real open PRs.
fn placeholder_reviews() -> Vec<Review> {
    vec![
        Review {
            number: Some(142),
            branch: "feat/add-search".into(),
            added: 284,
            removed: 31,
        },
        Review {
            number: None,
            branch: "fix/scroll-jitter".into(),
            added: 96,
            removed: 12,
        },
        Review {
            number: Some(139),
            branch: "refactor/row-model".into(),
            added: 1204,
            removed: 806,
        },
    ]
}
