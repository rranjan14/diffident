//! Orchestrator. Opens the window and wires crates together — no domain logic,
//! no rendering, no transport. If this file grows past wiring, the logic belongs
//! in a crate under `crates/`.

use diffident_model::{LoadState, Review, ReviewId};
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

/// Stand-in until Task H loads real open PRs. Kept compiling against the new
/// `Review` shape so Wave 1 can start.
fn placeholder_reviews() -> Vec<Review> {
    vec![
        Review {
            id: ReviewId {
                repo: "owner/name".into(),
                number: 142,
            },
            title: "Add search".into(),
            branch: "feat/add-search".into(),
            depth: 0,
            is_draft: false,
            state: LoadState::Ready {
                head_sha: "abc".into(),
                added: 284,
                removed: 31,
            },
        },
        Review {
            id: ReviewId {
                repo: "owner/name".into(),
                number: 140,
            },
            title: "Fix scroll jitter".into(),
            branch: "fix/scroll-jitter".into(),
            depth: 0,
            is_draft: false,
            state: LoadState::Ready {
                head_sha: "def".into(),
                added: 96,
                removed: 12,
            },
        },
        Review {
            id: ReviewId {
                repo: "owner/name".into(),
                number: 139,
            },
            title: "Refactor row model".into(),
            branch: "refactor/row-model".into(),
            depth: 0,
            is_draft: false,
            state: LoadState::Ready {
                head_sha: "ghi".into(),
                added: 1204,
                removed: 806,
            },
        },
    ]
}
