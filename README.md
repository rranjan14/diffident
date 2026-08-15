# diffident

A native code review app in Rust, built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

**One window, many reviews.** Existing desktop diff viewers open a separate OS window per
pull request, so reviewing a 4-PR stack means four windows to cycle between. diffident keeps
a persistent rail of open reviews in a single window, with stacked PRs rendered as nested
groups.

## Status

Early. The window, the review rail and crate scaffolding exist; the diff engine, GitHub
transport and review semantics do not yet. Not usable for real review work.

## Build

Requires macOS and Rust 1.97.1 (pinned in `rust-toolchain.toml` — Zed's `main` uses stdlib
features stabilized after 1.91).

```sh
cargo run
```

The first build is slow: `gpui` pulls ~60 transitive crates. Metal shaders compile at
runtime via the `runtime_shaders` feature, so a full Xcode install is not required.

```sh
cargo test --workspace   # note: --workspace, or only the binary is tested
cargo clippy --workspace --all-targets -- -D warnings
```

## Layout

```
src/main.rs      orchestrator — opens the window and wires crates. No logic.
crates/model     domain types. No dependencies, no gpui.
crates/ui        GPUI views. The only crate that knows a UI toolkit exists.
```

`crates/{diff,forge,session}` land next. None of the headless crates may depend on `gpui`;
`cargo tree -p diffident-diff | grep gpui` should print nothing.

## Prior art

[codiff](https://github.com/nkzw-tech/codiff) for visual density, and
[tuicr](https://github.com/agavra/tuicr) for review semantics — both worth your time.

## License

MIT
