# Local patch to servo-paint 0.8.0

Source: the published `servo-paint 0.8.0` crate, Servo tag `v0.8.0`,
`components/paint`.
MPL-2.0 headers and the normalized manifest are retained. Registry metadata,
the original manifest and the nested lockfile are omitted.

The renderer spreads the default action of accepted line-mode wheel input
over 120 ms using its `TimerRefreshDriver` and `Painter::perform_updates()`. It
preserves 40 logical pixels per line, dispatches the original wheel event once,
respects preventDefault(), and leaves pixel-mode input immediate. Reversing an
axis discards its old remaining movement. Keyboard, mouse-button, touch, scroll
bounds and explicit scrolling cancel the pending animation. It also normalizes
sub-1 px (`< 1.0`) layout rounding noise on scroll nodes before hit-test scroll
chaining so non-scrollable `<textarea>` elements do not consume wheel ticks,
and quantizes intermediate `WheelScroll` steps to `2^-10` px to prevent multi-frame
`f32` `-1 ULP` truncation below integer `window.scrollY`.

Only `lib.rs`, `painter.rs` and `webview_renderer.rs` differ from the published Rust files.
`wheel_scroll.rs` contains the private interpolation and five focused tests;
it has no external dependencies and can be tested directly with rustc.
The Windows Servo check in `servo-build.yml` type-checks the full patched crate
through the probe's Cargo manifest.
The full crate is present because Cargo patches apply at package level.
No engine version, dependency version or public API is added or changed.
