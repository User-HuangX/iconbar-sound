# Repository Guidelines

## Project Overview

`iconbar-sound` is a persistent, toggleable sound control panel for Wayland compositors: a GTK4 **layer-shell** overlay window (top-left anchored, floating at x≈1300) backed by **PulseAudio** (works under PipeWire-Pulse). It shows output/input device cards (selector, volume slider, mute switch, live mic level meter), refreshes state every 2 s, and toggles visibility via **SIGWINCH** or a second app activation. Single binary crate, edition 2024.

All user-facing strings and code comments are **Simplified Chinese** — keep new strings/comments in Chinese.

## Architecture & Data Flow

Two-thread actor model; the two source files are strict boundaries:

```
┌─ GTK main thread (src/main.rs) ──────────────┐   ┌─ worker thread (src/audio.rs) ─────────────┐
│ widgets, glib timeouts, CSS, layer-shell     │   │ PulseBackend: libpulse threaded Mainloop   │
│                                              │   │ + Context; owns all PulseAudio calls        │
│ commands: Sender<AudioCommand> ──────────────┼──►│ recv_timeout(75 ms): Refresh/Select/        │
│ events:   Receiver<AudioEvent>  ◄────────────┼───│ SetVolume/SetMute; Timeout → mic level     │
└──────────────────────────────────────────────┘   │ publishes Snapshot/Level/Error events       │
        poll_events: 40 ms glib timeout            └──────────────────────────────────────────────┘
```

- **Threading invariant (critical):** all libpulse calls MUST stay inside the worker thread — libpulse callbacks run while `mainloop.lock()` is held (see module doc, `src/audio.rs:1-3`). GTK handlers may only send `AudioCommand`s and read `AudioEvent`s. `src/audio.rs` is deliberately GTK-free; `src/main.rs` never touches libpulse.
- **Events:** GTK→worker via `AudioCommand` (`Refresh`, `Select{kind,index}`, `SetVolume{...}`, `SetMute{...}`, `#[allow(dead_code)] Stop`); worker→GTK via `AudioEvent` (`Snapshot(EndpointState)`, `Level(f64)`, `Error(String)`).
- **Timers:** 2 s refresh (`main.rs:441`), 40 ms event pump (`main.rs:444`), 16 ms SIGWINCH toggle poll (`main.rs:85`), 75 ms mic-level idle tick (`audio.rs:126`), 80 ms volume debounce re-armed on the last drag plus a 3 s give-up unlock (both `timeout_add_local_once` in `bind_endpoint`, `main.rs`).
- **UI state flow:** `Snapshot` events → `Controls::apply_snapshot` writes widgets guarded by an `updating` flag (echo-back suppression); mic `Level(f64)` → `ProgressBar` fraction.

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | `main.rs` (GTK UI + lifecycle) + `audio.rs` (PulseAudio boundary, GTK-free) |
| `src/resources/` | `style.css` — dark "forest night" theme, embedded at **compile time** via `include_str!` into a `CssProvider` (no gresource, no runtime path) |
| `.worktrees/audio-panel/` | Git worktree on `feat/audio-panel` holding an older app snapshot (pre-SIGWINCH toggle, old cyan theme); `audio.rs` there is identical to `main` |
| `target/` | Build artifacts (gitignored; stale fingerprints from prior dependency sets are residue, ignore) |

No `tests/`, `benches/`, `examples/`, `scripts/`, `docs/`, CI config, or README exist.

## Development Commands

```sh
cargo run            # debug build, run the panel (needs a Wayland session)
cargo run --release  # release build (1.0 MB vs 55 MB debug)
cargo test           # unit tests in src/audio.rs (no dev-dependencies needed)
```

No `[features]`, no `[profile]` overrides, no `build.rs`, no env vars affect the build. Wayland-only: `init_layer_shell()` renders nothing under X11, and the app **panics headless** (`main.rs:97`). System deps via pkg-config: `libgtk-4-dev`, `libpulse-dev`, layer-shell protocol.

## Code Conventions & Common Patterns

- **Naming:** standard Rust (`snake_case` fns/vars, `CamelCase` types, `SCREAMING_SNAKE` consts, kebab-case CSS classes).
- **Error handling:** no anyhow/thiserror. Fallible ops return `Result<T, String>` with Chinese messages (e.g. `"无法连接 PulseAudio / PipeWire-Pulse"`). Cross-thread errors become `AudioEvent::Error(String)` surfaced in a collapsible UI `Revealer`. Send failures are ignored with `let _ =`.
- **Panics:** `.expect()` only for signal registration and display availability; `.unwrap()` only inside pulse callbacks (poison-on-panic) and safe-by-construction slots (the panel `RefCell` slot). Device-list collection uses poison-tolerant `lock().unwrap_or_else(into_inner)` + `mem::take` instead of `Arc::try_unwrap().unwrap()` — the latter races the async closure teardown on the mainloop thread and has panicked the worker. No `unsafe` anywhere.
- **No async:** threads + `std::sync::mpsc` + `glib::timeout_add_local` polling. Do not introduce tokio/async-await.
- **State management:** GTK side uses `Rc<RefCell<…>>` / `Rc<Cell<…>>` cloned into `move` closures: `updating` (snapshot echo-back guard) and `pending: Cell<Option<f64>>` — the requested volume awaiting confirmation; the slider may only be written back once a snapshot echoes the requested value or the 3 s give-up timeout fires (确认式解锁). Cross-thread uses `Arc<AtomicBool>` (toggle flag) and `Arc<Mutex<Vec>>` (pulse callback collection). `AudioService` owns the channels.
- **GTK construction:** builder pattern (`.builder().orientation(…).spacing(…)…build()`) for containers, plain constructors + setters for leaves, `StringList` as `DropDown` model. **No** `.ui` files, `glib!` macros, or GObject subclasses.
- **Signals:** `connect_*` + `move` closures; snapshot-applying handlers early-return when `updating.get()` (echo-back guard); respect `pending` during volume drags (确认式解锁).
- **Volume semantics:** UI scale is 0–150% but commands/backend carry fractions 0.0–1.5, clamped in two places (worker `audio.rs:121`, `PulseBackend::volume` `audio.rs:252`); mic level clamps 0.0–1.0.
- **Dead code kept intentionally:** `AudioCommand::Stop` is `#[allow(dead_code)]` — only tests send it; the worker exits on channel disconnect.

## Important Files

- `src/main.rs` — app entry, `Panel`/`Controls`/`EndpointWidgets` structs, `install_css()`, `configure_layer_shell()`, `bind_controls()`/`bind_endpoint()`, `poll_events()`. Consts: `APP_ID = "top.hxdbk.gtk-layer-sound"`, `WINDOW_WIDTH/HEIGHT = 480/360`, `BIAS = 1300`.
- `src/audio.rs` — `AudioBackend` trait (the test seam), `AudioService::spawn()` / `::with_backend(factory)`, generic `worker<B>`, `PulseBackend` (refresh / select / set_volume / set_mute / `microphone_level`, plus `move_streams` which migrates all live streams on device select), `rms_level`.
- `src/resources/style.css` — theme; key classes: `.sound-window`, `.sound-panel`, `.endpoint-card`, `.device-selector`, `.mute-switch`, `.volume-scale`, `.input-level`, `.error-text`.
- `Cargo.toml` — the only config file (13 lines).

## Runtime/Tooling Preferences

- **Rust:** edition 2024 (rustc ≥ 1.85; machine has stable 1.94.1 via rustup). No `rust-toolchain` file, no user `~/.cargo/config.toml`.
- **Deps:** `gtk4 0.11.4`, `gtk4-layer-shell 0.8.0`, `libpulse-binding 2.30.1`, `signal-hook 0.3.18` (SIGWINCH flag); no `[dev-dependencies]`.
- **Git:** conventional-commit-style subjects (`feat:`/`fix:`/`style:`/`chore:`); worktrees live in `.worktrees/` (gitignored). Main branch tracks `origin/main`.
- **Unusual toggle design — preserve it:** visibility is driven by `signal_hook::flag::register(SIGWINCH)` + a 16 ms poll (`main.rs:26-28, 73-86`). Don't "simplify" it into something else.

## Testing & QA

- **Location:** `#[cfg(test)] mod tests` in `src/audio.rs:613-785` — the only tests. Six unit tests, all using a `FakeBackend` mock (`audio.rs:616-668`) injected via `AudioService::with_backend`: `rms_is_normalized`, `endpoint_state_preserves_kind`, `worker_publishes_fake_backend_snapshot`, `worker_routes_output_input_commands_and_clamps_volume` (asserts 9.0 clamps to 1.5), `backend_error_keeps_last_snapshot`. Channel assertions use 1 s `recv_timeout`.
- **Run:** `cargo test`. No CI, no coverage tooling, no integration tests (GTK UI is untested; no display in test envs).
- **Test seam contract:** `AudioBackend` trait + generic worker exist for testing — keep the trait and `worker<B>` generic or the tests break. New backend logic should go behind the trait; pure helpers (like `rms_level`) are unit-testable without a Pulse server; `PulseBackend` I/O methods need a live server.
