//! The GPUI layer. The only crate besides the binary that links gpui.

pub mod comment_view;
pub mod composer;
pub mod density;
pub mod diff_view;
pub mod file_list;
pub mod images;
pub mod loader;
pub mod navigate;
pub mod palette;
pub mod rail;
pub mod residency;
pub mod search;
pub mod sidebar;
pub mod split;
pub mod submit;
pub mod theme;
pub mod suggest;
pub mod threads;
pub mod workspace;

pub use workspace::Workspace;
