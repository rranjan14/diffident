---
name: gpui-conventions
description: Use when writing, reviewing, or debugging any GPUI code in diffident — UI elements, windows, lists, text rendering, actions/keymaps, background work — or when touching Cargo.toml, Cargo.lock, rust-toolchain.toml, or considering a new UI dependency. Covers the build constraints that silently break (font-kit, toolchain floor), the Zed licensing boundary, and which GPUI primitives actually exist.
---

# GPUI conventions (diffident)

## Build constraints — these fail in non-obvious ways

- `gpui` + `gpui_platform` are **git deps on `zed-industries/zed` main**. crates.io `gpui`
  0.2.2 predates the `gpui`/`gpui_platform` split and `gpui_platform` was never published,
  so the upstream README snippet does not resolve. Don't "fix" this by switching to
  crates.io.
- **`font-kit` feature is required on macOS.** Without it text lays out fine and renders
  **zero glyphs** — a blank window, no error.
- Keep `runtime_shaders`. It compiles Metal shaders at runtime and drops the full-Xcode
  build requirement. Removing it breaks CI and clean machines even though Xcode is
  installed here.
- **`Cargo.lock` is the pin.** The git dep is unpinned `main`; the committed lockfile is
  the only thing holding it. Never delete it, never `cargo update` casually. Bump
  deliberately, in its own commit.
- `rust-toolchain.toml` pins **1.97.1**. Zed main uses stdlib features stabilized after
  1.91 (e.g. `gpui_util` calls `slice_as_array` → E0658 on 1.91.1). Bumping gpui can raise
  this floor again — expect to move both together. Don't mutate the machine's global
  toolchain instead.

Escape hatch if git churn hurts: `gpui-unofficial` on crates.io (auto-republished from Zed
release tags). Mixing it with gpui-component needs `[patch]` blocks.

## Licensing boundary — do not cross

`gpui` is Apache-2.0. **Everything Zed built on top of it — `ui`, `workspace` (panes,
splits, docks, tab bar), `editor` — is GPL-3.0-or-later and `publish = false`.** Never
vendor, copy, or paste from those crates. Reading them for ideas is fine; lifting code is
not.

Apache-2.0 sources we may use: `gpui` itself, `gpui-component`. Anything else, check first.

## What GPUI does *not* give you

No tab bar, no pane splitter, no scrollbar widget, no editor primitive. If you reach for
one, you are about to hand-roll it or add a dependency — say so rather than assuming it
exists.

Available primitives: `div`, `uniform_list`, `list`, `canvas`, `deferred`, `anchored`,
`img`, `svg`, `StyledText`, `InteractiveText`, `ScrollHandle`.

Default: hand-roll on `div` + `uniform_list`. A sidebar and a two-pane split are flex
boxes. `gpui-component` is a large second unpinned git source — pull it in only when we
actually want resizable/dockable panels (Phase 7), not before.

## Rendering model

One flat `Vec<Row>` is the **sole source of truth** for rendering, hit-testing, navigation
and scroll math. Index parity between that vector and the list element is the whole point —
keep the model element-agnostic so the element can be swapped under it.

- `uniform_list` is **fixed row height only**. Fine for diff lines with wrap off; breaks
  for inline comment rows and wrapped lines.
- When variable height becomes necessary, switch to `list(ListState, render_item)` (needs
  `measure_all()`). The `Vec<Row>` model survives untouched — only the element call changes.
- Scrollbars are hand-rolled: `canvas` + `UniformListScrollHandle::set_offset`. See
  `examples/data_table.rs`.
- Syntax highlighting: `StyledText::with_default_highlights(&style, ranges)` where ranges
  are sorted, non-overlapping **byte** ranges on char boundaries.

## Async work

Use GPUI's own executor: `cx.background_executor().spawn(…)` for blocking/subprocess work,
`cx.spawn` to land results on the main thread, then `cx.notify()`. No extra thread pools,
no mpsc poll loops — those are ratatui workarounds.

Tag every in-flight request with its identity tuple `(repo, pr, head_sha)` and **drop late
results whose tuple no longer matches** the current selection.

## Actions and keys

`actions!` + `cx.bind_keys`, scoped with `.key_context("Diff")` / `("Rail")`. Bind at
startup inside `run()`. Keep the key→action mapping a pure function so it is unit-testable
without a window.

## Docs are thin — read the examples

GPUI's written docs are sparse. The source of truth is `crates/gpui/examples/` in the Zed
checkout: `input.rs` (text/focus/actions), `data_table.rs` (virtual list + scrollbar),
`list_example.rs`, `popover.rs`, `ownership_post.rs`, `testing.rs`.

## Keep it headless where you can

`forge/` and `diff/` are pure and compile/test without a window. Logic that can live there
should — `cargo test` there is cheap; testing through a window is not.
