//! The GPUI layer. The only crate besides the binary that links gpui.

pub mod diff_view;
pub mod file_list;
pub mod loader;
pub mod navigate;
pub mod rail;
pub mod residency;
pub mod scrollbar;
pub mod theme;
pub mod workspace;

pub use workspace::Workspace;
