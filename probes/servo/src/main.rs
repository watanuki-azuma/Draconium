/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

mod shot;
mod starter;
mod workspace;

use std::error::Error;
use std::path::{Path, PathBuf};

use draconium_host::platform;
use draconium_host::{
    Capabilities, DevOrigin, EmbeddedAsset, HostConfig, HostHandler, LogicalSize, Url,
    WindowConfig, resolve_app_url_from, resolve_ui_path_from, run,
};

use crate::shot::ShotApp;
use crate::starter::StarterApp;
use crate::workspace::{WorkspaceApp, default_workspace_dir};

/// Compiled-in UI fallback assets for single-binary `dn://app/` delivery.
const EMBEDDED_ASSETS: &[EmbeddedAsset] = &[
    EmbeddedAsset {
        path: "workspace.html",
        bytes: include_bytes!("../ui/workspace.html"),
    },
    EmbeddedAsset {
        path: "workspace.css",
        bytes: include_bytes!("../ui/workspace.css"),
    },
    EmbeddedAsset {
        path: "workspace.js",
        bytes: include_bytes!("../ui/workspace.js"),
    },
    EmbeddedAsset {
        path: "starter.html",
        bytes: include_bytes!("../ui/starter.html"),
    },
    EmbeddedAsset {
        path: "starter.css",
        bytes: include_bytes!("../ui/starter.css"),
    },
    EmbeddedAsset {
        path: "starter.js",
        bytes: include_bytes!("../ui/starter.js"),
    },
    EmbeddedAsset {
        path: "shot.html",
        bytes: include_bytes!("../ui/shot.html"),
    },
    EmbeddedAsset {
        path: "shot.css",
        bytes: include_bytes!("../ui/shot.css"),
    },
    EmbeddedAsset {
        path: "shot.js",
        bytes: include_bytes!("../ui/shot.js"),
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Workspace,
    Starter,
    Probe,
    Shot,
}

impl Mode {
    fn mutex_name(self) -> &'static str {
        match self {
            Mode::Workspace => "Local\\Draconium.Workspace",
            Mode::Starter => "Local\\Draconium.Starter",
            Mode::Probe => "Local\\Draconium.Probe",
            Mode::Shot => "Local\\Draconium.Shot",
        }
    }

    fn autostart_app_name(self) -> &'static str {
        match self {
            Mode::Workspace => "Draconium.Folio",
            Mode::Starter => "Draconium.Starter",
            Mode::Probe => "Draconium.Probe",
            Mode::Shot => "Draconium.Shot",
        }
    }

    fn autostart_args(self) -> Vec<&'static str> {
        match self {
            Mode::Workspace => vec!["--workspace", "--single-instance"],
            Mode::Starter => vec!["--starter", "--single-instance"],
            Mode::Probe => vec!["--probe"],
            Mode::Shot => vec!["--shot", "--single-instance"],
        }
    }

    fn capabilities(self) -> Capabilities {
        match self {
            Mode::Workspace => Capabilities::standard_desktop()
                .with_autostart(true)
                .with_site_data(true)
                .with_app_commands([
                    "list_notes",
                    "load_note",
                    "save_note",
                    "create_note",
                    "delete_note",
                    "import_note",
                    "record_benchmark",
                ]),
            Mode::Starter => Capabilities::restricted()
                .with_window_sizing(true)
                .with_capture(true)
                .with_app_commands(["echo", "host_info"]),
            Mode::Probe => Capabilities::restricted().with_capture(true),
            Mode::Shot => Capabilities::restricted()
                .with_window_sizing(true)
                .with_file_dialogs(true)
                .with_notifications(true)
                .with_clipboard(false, true)
                .with_app_commands([
                    "capture_screen",
                    "get_settings",
                    "save_settings",
                    "save_shot",
                ]),
        }
    }
}

impl HostHandler for WorkspaceApp {
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        WorkspaceApp::handle_request(self, command, payload)
    }
}

impl HostHandler for StarterApp {
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        StarterApp::handle_request(self, command, payload)
    }
}

impl HostHandler for ShotApp {
    fn handle_request(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command {
            // Desktop capture is a host platform primitive (the window
            // `capturePng` command only photographs the page itself), so it
            // is intercepted here to keep `shot.rs` std-only.
            "capture_screen" => platform::capture_desktop_image_json(),
            _ => ShotApp::handle_request(self, command, payload),
        }
    }
}

struct ProbeHandler;

impl HostHandler for ProbeHandler {
    fn handle_request(&mut self, command: &str, _payload: &str) -> Result<String, String> {
        Err(format!("Unsupported command in probe mode: {command}"))
    }
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn file_url(exe_relative: &str, manifest_relative: &str) -> Url {
    let path = resolve_ui_path_from(manifest_dir(), exe_relative, manifest_relative)
        .canonicalize()
        .unwrap_or_else(|err| panic!("UI file missing ({exe_relative}): {err}"));
    Url::from_file_path(path).expect("Invalid UI file path")
}

struct CliArgs {
    mode: Mode,
    workspace_dir: Option<PathBuf>,
    crash_dir: Option<PathBuf>,
    self_test: Option<String>,
    dn_assets: bool,
    csp_wasm: bool,
    dev_url: Option<String>,
    dev_origin: Option<DevOrigin>,
    deep_link: Option<String>,
    raw_args: Vec<String>,
    tray: bool,
    screenshot: Option<PathBuf>,
    transparent: bool,
    opacity: Option<f64>,
    hotkey: Option<(u32, u32)>,
    single_instance: bool,
    devtools_addr: Option<String>,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut mode = Mode::Workspace;
    let mut workspace_dir = None;
    let mut crash_dir = None;
    let mut self_test = None;
    let mut dn_assets = false;
    let mut csp_wasm = false;
    let mut dev_url = None;
    let mut dev_origin = None;
    let mut deep_link = None;
    let mut tray = false;
    let mut screenshot = None;
    let mut transparent = false;
    let mut opacity = None;
    let mut hotkey = None;
    let mut single_instance = false;
    let mut devtools_addr = None;
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let mut args = raw_args.iter().cloned();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => mode = Mode::Workspace,
            "--starter" => mode = Mode::Starter,
            "--probe" => mode = Mode::Probe,
            "--shot" => mode = Mode::Shot,
            "--dn-assets" => dn_assets = true,
            "--csp-wasm" => csp_wasm = true,
            "--tray" => tray = true,
            "--transparent" => transparent = true,
            "--single-instance" => single_instance = true,
            "--dev-url" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--dev-url requires a loopback http:// URL".to_owned())?;
                let origin = DevOrigin::parse(&value)?;
                let _ =
                    Url::parse(&value).map_err(|e| format!("--dev-url is not a valid URL: {e}"))?;
                dev_url = Some(value);
                dev_origin = Some(origin);
            }
            "--deep-link" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--deep-link requires a URI".to_owned())?;
                let Some((scheme, _)) = value.split_once("://") else {
                    return Err(format!("--deep-link must be a <scheme>:// URI: {value}"));
                };
                let _ = platform::validate_custom_uri_scheme(scheme)?;
                deep_link = Some(value);
            }
            "--crash-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--crash-dir requires a directory path".to_owned())?;
                crash_dir = Some(PathBuf::from(value));
            }
            "--opacity" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--opacity requires a float in 0.1..=1.0".to_owned())?;
                let parsed: f64 = value
                    .parse()
                    .map_err(|_| format!("--opacity must be a number: {value}"))?;
                if platform::opacity_to_alpha(parsed).is_none() {
                    return Err(format!("--opacity must be finite: {value}"));
                }
                opacity = Some(parsed);
            }
            "--hotkey" => {
                let value = args.next().ok_or_else(|| {
                    "--hotkey requires an accelerator (e.g. Ctrl+Alt+Q)".to_owned()
                })?;
                hotkey = Some(platform::parse_hotkey(&value)?);
            }
            "--devtools" => {
                devtools_addr = Some("127.0.0.1:7000".to_owned());
            }
            "--devtools-addr" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--devtools-addr requires host:port".to_owned())?;
                devtools_addr = Some(platform::validate_devtools_addr(&value)?);
            }
            "--workspace-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--workspace-dir requires a path".to_owned())?;
                workspace_dir = Some(PathBuf::from(value));
            }
            "--self-test" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--self-test requires a test name".to_owned())?;
                self_test = Some(value);
            }
            "--screenshot" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--screenshot requires an output PNG path".to_owned())?;
                screenshot = Some(PathBuf::from(value));
            }
            other => {
                if !other.starts_with('-')
                    && let Some((scheme, _)) = other.split_once("://")
                    && platform::validate_custom_uri_scheme(scheme).is_ok()
                {
                    deep_link = Some(other.to_owned());
                } else {
                    return Err(format!("Unknown argument: {other}"));
                }
            }
        }
    }

    Ok(CliArgs {
        mode,
        workspace_dir,
        crash_dir,
        self_test,
        dn_assets,
        csp_wasm,
        dev_url,
        dev_origin,
        deep_link,
        raw_args,
        tray,
        screenshot,
        transparent,
        opacity,
        hotkey,
        single_instance,
        devtools_addr,
    })
}

fn apply_cli_window(base: WindowConfig, cli: &CliArgs) -> WindowConfig {
    base.with_tray(cli.tray)
        .with_transparent(base.transparent || cli.transparent)
        .with_opacity(cli.opacity)
        .with_global_hotkey(cli.hotkey)
}

fn resolve_crash_dir(cli: &CliArgs, base_data_dir: &Path) -> PathBuf {
    cli.crash_dir
        .clone()
        .unwrap_or_else(|| base_data_dir.join("crashes"))
}

fn build_host_config(
    title: &'static str,
    url: Url,
    asset_root: Option<PathBuf>,
    window: WindowConfig,
    crash_dir: PathBuf,
    cli: &CliArgs,
) -> HostConfig {
    HostConfig::new(title, url)
        .with_window(window)
        .with_asset_root(asset_root)
        .with_embedded_assets(EMBEDDED_ASSETS)
        .with_capabilities(cli.mode.capabilities())
        .with_crash_dir(crash_dir)
        .with_single_instance(cli.mode.mutex_name(), cli.single_instance)
        .with_autostart(cli.mode.autostart_app_name(), cli.mode.autostart_args())
        .with_devtools(cli.devtools_addr.clone())
        .with_screenshot(cli.screenshot.clone())
        .with_deep_link(cli.deep_link.clone())
        .with_raw_args(cli.raw_args.clone())
        .with_dev_origin(cli.dev_origin.clone())
        .with_wasm_csp(cli.csp_wasm)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut cli = match parse_args() {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    if cli.mode == Mode::Shot {
        // Draco Shot is a tray-resident tool: force the resident flags, and
        // fall back to the configured (or built-in default) hotkey when the
        // CLI did not specify one.
        cli.tray = true;
        cli.single_instance = true;
        if cli.hotkey.is_none() {
            let spec = shot::configured_hotkey(&shot::default_shot_dir());
            cli.hotkey = platform::parse_hotkey(&spec)
                .ok()
                .or_else(|| platform::parse_hotkey(shot::DEFAULT_HOTKEY).ok());
        }
    }

    match cli.mode {
        Mode::Workspace => {
            let data_dir = default_workspace_dir(cli.workspace_dir.clone());
            let (url, asset_root) = resolve_app_url_from(
                manifest_dir(),
                cli.dn_assets,
                cli.dev_url.as_deref(),
                "workspace.html",
                "ui/workspace.html",
                "ui/workspace.html",
            );
            let config = build_host_config(
                "Draconium Folio",
                url,
                asset_root,
                apply_cli_window(WindowConfig::custom_chrome(1240, 800), &cli),
                resolve_crash_dir(&cli, &data_dir),
                &cli,
            );
            run(config, WorkspaceApp::new(data_dir), cli.self_test)
        }
        Mode::Starter => {
            let base_dir = default_workspace_dir(None);
            let (url, asset_root) = resolve_app_url_from(
                manifest_dir(),
                cli.dn_assets,
                cli.dev_url.as_deref(),
                "starter.html",
                "ui/starter.html",
                "ui/starter.html",
            );
            let config = build_host_config(
                "Draconium Starter",
                url,
                asset_root,
                apply_cli_window(WindowConfig::standard(760, 560), &cli),
                resolve_crash_dir(&cli, &base_dir),
                &cli,
            );
            run(config, StarterApp::default(), cli.self_test)
        }
        Mode::Shot => {
            let data_dir = shot::default_shot_dir();
            let (url, asset_root) = resolve_app_url_from(
                manifest_dir(),
                cli.dn_assets,
                cli.dev_url.as_deref(),
                "shot.html",
                "ui/shot.html",
                "ui/shot.html",
            );
            // Fullscreen overlay window: frameless, transparent, always on
            // top, hidden until the hotkey (or tray) summons it.
            let window = WindowConfig {
                decorations: false,
                always_on_top: true,
                skip_taskbar: true,
                transparent: true,
                start_hidden: true,
                initial_size: LogicalSize::new(520, 420),
                ..WindowConfig::default()
            };
            let config = build_host_config(
                "Draco Shot",
                url,
                asset_root,
                apply_cli_window(window, &cli),
                resolve_crash_dir(&cli, &data_dir),
                &cli,
            );
            run(config, ShotApp::new(data_dir), cli.self_test)
        }
        Mode::Probe => {
            if cli.dn_assets {
                eprintln!(
                    "[info] --probe opens the shared comparison page (ui/index.html) via file://"
                );
            }
            let base_dir = default_workspace_dir(None);
            let url = if let Some(ref raw_dev) = cli.dev_url {
                Url::parse(raw_dev).expect("validated --dev-url parses")
            } else {
                file_url("ui/index.html", "../../ui/index.html")
            };
            let config = build_host_config(
                "Draconium - Servo",
                url,
                None,
                apply_cli_window(WindowConfig::default(), &cli),
                resolve_crash_dir(&cli, &base_dir),
                &cli,
            );
            run(config, ProbeHandler, cli.self_test)
        }
    }
}
