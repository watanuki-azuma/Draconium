# Optional Native Media (`media-gstreamer`)

QuantumView follows Servo 0.8.0's media backend selection. The default build uses `DummyBackend`; opting into the `media-gstreamer` Cargo feature selects `GStreamerBackend` for native `<audio>` and `<video>` media. The default Windows executable remains about 112 MiB and self-contained, with zero GStreamer runtime DLLs. The feature adds a graph of 39 optional crates recorded in `probes/servo/Cargo.lock`; they are not compiled by default, so default `--frozen` builds are unchanged.

## Enable the feature

For an application that depends on the host library, enable the feature on that dependency:

```toml
[dependencies]
quantumview-host = { path = "../QuantumView/crates/quantumview-host", features = ["media-gstreamer"] }
```

To build the Servo showcase binary with the checked-in lockfile:

```sh
cargo build --release --locked --features media-gstreamer --manifest-path probes/servo/Cargo.toml
```

The optional `media_gstreamer` input on the `Servo Release Build` workflow enables the feature for the Servo and host example builds. Linux and macOS install the GStreamer development packages. Stock GitHub `windows-latest` runners do not preinstall the GStreamer MSVC SDK. Provision the official x64 MSVC runtime and development SDK and put `pkg-config` on `PATH` before enabling the Windows job. The workflow adds `$GSTREAMER_1_0_ROOT_MSVC_X86_64\lib\pkgconfig` to `PKG_CONFIG_PATH` on Windows and checks both `gstreamer-1.0` and `gstreamer-play-1.0` with `pkg-config` on all three platforms before Cargo runs.

## Runtime feature detection

The feature state is reported by the host at runtime:

```js
const result = window.QuantumView.window.getPlatform();
if (result.ok && result.data.mediaGstreamer) {
  // This executable was compiled with the media-gstreamer feature.
}
```

Rust callers can use the same compiled-feature flag:

```rust
let media_enabled = quantumview_host::platform::platform_capabilities().media_gstreamer;
```

The value indicates that the feature was compiled in. It does not guarantee that the runtime, plugins, or a particular codec are installed.

## Build and runtime prerequisites

Use GStreamer **1.20 or later**. Servo 0.8.0's `servo-media-gstreamer` backend uses the `gstreamer-play` bindings and the `GstPlay` API introduced in GStreamer 1.20.

### Windows x64

- Build with the GStreamer 1.20+ MSVC runtime and development installers, and make `pkg-config` available to Cargo. Set `GSTREAMER_1_0_ROOT_MSVC_X86_64` to the SDK root and prepend its `lib\pkgconfig` directory to `PKG_CONFIG_PATH`.
- At runtime, place the GStreamer core runtime DLLs and Servo 0.8.0's 36 required plugin DLLs next to `quantumview-servo.exe`. The plugin filenames are `gst*.dll`.
- Package the files from an SDK install with:

  ```powershell
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 -GStreamerDir 'C:\gstreamer\1.0\msvc_x86_64'
  ```

  The helper copies core DLLs from `bin\` (or from the root for a flat directory), then finds the required plugins under `lib\gstreamer-1.0\`, `bin\`, or the root. The helper checks for a core runtime DLL and all 36 plugins before staging. `-AllowIncompleteGStreamer` allows missing plugins with a warning. Packaging replaces the destination only after the new bundle is ready, so a later package without `-GStreamerDir` drops the previous media DLLs.

### macOS

- Install GStreamer for building and runtime with `brew install gstreamer`; ensure its pkg-config files are visible when building.
- Servo 0.8.0 looks for the 38 required plugin `.dylib` files under `<exe_dir>/lib/`. GStreamerBackend also needs the GStreamer runtime libraries discoverable on the machine.

### Linux

- Install the development and runtime plugins before building and running. On Ubuntu/Debian, the relevant packages are:

  ```sh
  sudo apt-get install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
  ```

- Servo 0.8.0 initializes the Linux GStreamer backend through GStreamer's normal system plugin discovery. Install the runtime plugins system-wide; no app-local plugin list is loaded on Linux.

## Servo 0.8.0 plugin set and loading behavior

Servo 0.8.0's `components/servo/gstreamer_plugin_lists/common.rs.in` lists 35 common plugins:

`gstcoreelements`, `gstnice`, `gstapp`, `gstaudioconvert`, `gstaudioresample`, `gstgio`, `gstogg`, `gstopengl`, `gstopus`, `gstplayback`, `gsttheora`, `gsttypefindfunctions`, `gstvideoconvertscale`, `gstvolume`, `gstvorbis`, `gstaudiofx`, `gstaudioparsers`, `gstautodetect`, `gstdeinterlace`, `gstid3demux`, `gstinterleave`, `gstisomp4`, `gstmatroska`, `gstrtp`, `gstrtpmanager`, `gstvideofilter`, `gstvpx`, `gstwavparse`, `gstaudiobuffersplit`, `gstdtls`, `gstid3tag`, `gstproxy`, `gstvideoparsersbad`, `gstwebrtc`, and `gstlibav`.

Servo adds `gstwasapi` on Windows, for 36 plugins total, and `gstosxaudio`, `gstosxvideo`, and `gstapplemedia` on macOS, for 38 total. Windows uses `<exe_dir>/gst*.dll`; macOS uses `<exe_dir>/lib/libgst*.dylib`.

On Windows and macOS, Servo 0.8.0 calls `GStreamerBackend::init_with_plugins` with the executable-relative plugin directory and platform list. If that initialization fails, Servo logs a warning and calls `GStreamerBackend::init()` as a fallback, letting GStreamer use its normal system/runtime plugin discovery. Linux directly initializes `GStreamerBackend` and uses normal system plugin discovery.

## Distribution trade-offs and scope

Bundling GStreamer makes native media available without requiring a separate install, but increases distribution size and adds runtime DLLs or shared libraries to maintain. Using a machine's GStreamer installation keeps the app package smaller but makes media behavior depend on the installed GStreamer version, plugins, and codecs. Check the distribution terms for each plugin and codec you ship, and meet any applicable licensing obligations for formats such as H.264 and AAC.

Widevine and EME DRM playback are outside this feature's scope.

## Verification status

The default build with `media-gstreamer` off is verified on Windows, and the CI verification workflow runs on `ubuntu-latest`, `macos-latest`, and `windows-latest`. The opt-in feature's compilation and playback have not been verified here; they require a host with a GStreamer 1.20+ development SDK and `pkg-config` (plus the OS runtime prerequisites above).
