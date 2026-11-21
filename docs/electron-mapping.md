# Electron 機能と Chromium 限定機能の Servo + SpiderMonkey での実装方法

対象: Servo 0.8.0（Stylo 0.23 / WebRender 0.70 / SpiderMonkey）、winit 0.30.13、Windows 先行。
分類: **実装可** = エンジン無変更・公開 API のみでホスト実装可能 / **エンジン変更** = `vendor/` または上流 Servo の変更が必要 / **対象外** = KISS/YAGNI・方針上やらない。

## Cross-Platform Window Controls

`WindowConfig::with_min_size(Option<HostLogicalSize>)` and
`WindowConfig::with_max_size(Option<HostLogicalSize>)` set logical inner-size
limits when the native window is created.

`QuantumView.window` provides `setTitle`, `setFullscreen`, `isFullscreen`,
`setAlwaysOnTop`, `focus`, `requestUserAttention`, `getBounds`, `setSize`,
`goBack`, `goForward`, `canGoBack`, and `canGoForward`. Fullscreen changes are
reported through `QuantumView.on("fullscreen-changed", listener)` with a
`"true"` or `"false"` payload. Pressing **F11** toggles borderless fullscreen
when the app has the `WindowSizing` capability.

## 前提（アーキテクチャ対応）

| Electron | QuantumView |
| --- | --- |
| メインプロセス（Node.js、特権） | Rust ホストプロセス自体（`host.rs`）。Node は存在しない（方針） |
| レンダラ（Chromium/Blink + V8） | Servo `WebView`（SpiderMonkey + Servo レイアウト/WebRender） |
| preload（分離ワールド、`contextIsolation`） | `UserContentManager` の `UserScript` 注入。**ただしメインワールド同一 realm**（後述） |
| `ipcMain.handle` / `ipcRenderer.invoke` | ホスト定義コマンドの同期ブリッジ（`SimpleDialog::Prompt` 経由）＋ 非ブロッキング相関非同期ブリッジ（`QuantumView.invokeAsync` ↔ `__quantumviewResolveAsync`）＋ `WebView::evaluate_javascript` によるホスト→ページ通知 |

## 1. Electron API モジュール別対応

| Electron モジュール | Servo / Web 側の受け皿 | ホスト実装方針 | 分類 |
| --- | --- | --- | --- |
| `app`（lifecycle、quit 抑止、単一インスタンス＋引数転送、カスタム URI 登録、自動起動、パス取得） | なし | winit イベントループが lifecycle。終了前フックは既存 `__quantumviewBeforeClose` 評価が `before-quit` 相当。単一インスタンスは `--single-instance` で Win32 名前付き Mutex（`CreateMutexW`）＋ 2 番目起動時の隠しイベントウィンドウ検索（`FindWindowExW`）＆ `WM_COPYDATA` 転送（`second-instance` / `open-url` イベント配信）、カスタム URI スキーム（`HKCU\Software\Classes\<scheme>`、`QuantumView.protocolClient` / `--deep-link`）、自動起動（`HKCU\...\Run`、`QuantumView.autostart`）をすべて実装済み | 実装可（実装済み） |
| `BrowserWindow`（生成・表示・移動・リサイズ・最小化・子ウィンドウ等） | `WebView::load/reload/go_back/go_forward`、delegate の `request_move_to` / `request_resize_to` / `request_create_new` 等 | winit `Window` の各 API に 1:1 対応。ページ要求の move/resize はホスト側でモニタ可視判定・サイズクランプ付きで実装済み。`QuantumView.window.openChild` および `window.open`（`qv://app/...`・ローカル `file://` のみ）で権限減衰済み（`Capabilities::attenuate_for_child()`）の補助 `Window` + `WindowRenderingContext` + `WebView`（最大 4 枚、`State` の drop 順保護付き）を生成可能（実装済み） | 実装可（実装済み） |
| `BrowserWindow`（frame:false / alwaysOnTop / skipTaskbar） | なし | `with_decorations(false)`、`with_window_level(AlwaysOnTop)`、`WindowAttributesExtWindows::with_skip_taskbar(true)`＋枠なし時の Win11 DWM 角丸（`DWMWA_WINDOW_CORNER_PREFERENCE`）を実装済み | 実装可（実装済み） |
| `BrowserWindow`（transparent / opacity / click-through） | `servo-paint-api` は既定で `ContextAttributeFlags::ALPHA` を要求し、`Preferences.shell_background_color_rgba` で WebRender / GL クリア色を指定可能 | `--transparent` および `WindowConfig::custom_chrome` で `with_transparent(true)`＋`shell_background_color_rgba = [0,0,0,0]` を設定。加えて Win32 `WS_EX_LAYERED`＋`SetLayeredWindowAttributes`（`--opacity`・`window.setOpacity`）と `WS_EX_TRANSPARENT`（`window.setClickThrough`）を `win_platform.rs` で実装済み | 実装可（実装済み） |
| `-webkit-app-region: drag`（CSS ドラッグ領域） | Chromium 限定 CSS。Servo にプロパティも delegate 通知もなし | プリロードの `mousedown` シム（`bridge.rs`）で `[data-qv-drag]`・インライン `-webkit-app-region: drag`・`--qv-app-region: drag` を自動検出し、インタラクティブ要素・`no-drag` を除外して `window.drag()` を呼び出す（実装済み） | 代替実装済み |
| ウィンドウ位置/サイズ永続化 | なし（Electron でも自前） | `WindowEvent::Moved` をホストが受け、最小化異常座標・起動直後配置を除外してアプリ側ハンドラへ通知。復元時はウィンドウ矩形の可視面積（32x32 px 以上）を検証（実装済み） | 実装可（実装済み） |
| `webContents.executeJavaScript` / reload / ignoreCache / zoom / 履歴 | `WebView::evaluate_javascript`（結果は `JSValue`）、`reload()`、`Servo::network_manager().clear_cache()`、`set_page_zoom(f32)`（0.1〜10.0）、`can_go_back/forward` | ガード付きリロード（`F5`/`Ctrl+R`）、キャッシュ消去付きハードリロード（`Ctrl+Shift+R`/`Ctrl+F5`・`window.hardReload()`）、ズーム（`Ctrl+=`/`-`/`0`・`window.zoomIn/Out/Reset`、0.5〜3.0 クランプ）を実装済み。JS 文字列への未検証データ埋め込みは禁止（JSON リテラル経由） | 実装可（実装済み） |
| `webContents` 遷移ガード（`will-navigate` 等） | delegate `request_navigation`（`allow()`/`deny()`、応答なしで drop すると許可） | ローカルアプリ前提でローカル `file://`（空/`localhost` authority のみ、UNC 拒否）・`qv://app/`・`about:blank`、および開発時の `--dev-url http://127.0.0.1:<port>` ループバック origin のみに制限（実装済み） | 実装可（実装済み） |
| `webContents.capturePage` | `WebView::take_screenshot(rect, callback)`。読込・描画完了を待って**生ピクセル `RgbaImage`** が返る | `--screenshot <path>` に加え、対話用の `QuantumView.window.capturePng(name?)`（`GetSaveFileNameW`→`take_screenshot`→ワーカースレッド PNG 保存→`capture-saved` 通知）を実装済み | 実装可（実装済み） |
| `webContents` DevTools / CDP | Chrome DevTools・CDP は存在しない。Servo は **Firefox DevTools プロトコル（RDP）サーバ**（pref `devtools_server_enabled`、既定 `127.0.0.1:7000`） | `show_console_message` 転送＋リロード＋`--screenshot` に加え、`--devtools` / `--devtools-addr`（ループバック限定検証付き）と `--dev-url`（ローカル開発サーバ HMR 接続）を配線・実機検証済み | CDP 互換は対象外、代替は実装済み |
| `webContents` 印刷 / `printToPDF` / `savePage` | 印刷系 API・delegate が存在しない | `QuantumView.window.exportPdf(name?)`（`take_screenshot` の RGBA フレームから純 Rust で `%PDF-1.4` `/FlateDecode` 単一ページラスタ PDF をワーカースレッド生成・保存）、`QuantumView.window.exportHtml(name?)`（シリアライズ HTML 保存）、および `window.print()` → `exportPdf` フォールバックを実装済み（ベクタ PDF はエンジン変更枠） | ホスト代替実装済み（ラスタ PDF＋HTML 出力） |
| `webContents` ダウンロード（`will-download` 等） | ダウンロード用 delegate/型が存在しない | プリロードで `<a download href="data:...|blob:...">` クリックを横取りし、base64 変換→`qv-window:save-download`→Win32 `GetSaveFileNameW` でネイティブ保存する代替経路を実装済み（一般的な HTTP ダウンロードはエンジン変更枠） | ローカル出力代替実装済み |
| `protocol.handle`（カスタムスキーム） | `servo::protocol_handler::{ProtocolHandler, ProtocolRegistry}` ＋ `ServoBuilder::protocol_registry(...)` | `qv://app/` を常時登録（`qv_protocol.rs` ＋ `protocol.rs`）。多階層相対バンドルパス（`assets/chunks/...`、`fonts/...`、`wasm/...`）の正規化と `canonicalize()` ルート閉じ込め、拡張 MIME 表（`wasm`/`woff2`/`ttf`/`otf`/`webp`/`avif`/`mp3`/`wav`/`ogg`/`mp4`/`webm`/`map`/`md`）、`include_bytes!` による単一バイナリ埋め込みフォールバック、HTML 応答への `Content-Security-Policy`（`--csp-wasm` 対応）と `X-Content-Type-Options: nosniff` 注入を実装済み | 実装可（実装済み） |
| `protocol` の代替としての `load_web_resource` | delegate `load_web_resource` ＋ `WebResourceLoad::intercept(...)` | 公式契約は HTTP/HTTPS リソースの横取り。カスタムスキーム配信の代替にしない | 実装可（用途限定） |
| `ipcMain` / `ipcRenderer` / `contextBridge` | 組み込み機構なし | 同期 `QuantumView.invoke` に加え、長時間のファイル・OS 操作でもレンダラを止めない相関 ID 付き非ブロッキング `QuantumView.invokeAsync`（`qv-async:` / `qv-app-async:` → `window.__quantumviewResolveAsync`）とアプリ毎の宣言的 `Capabilities` ACL（`policy.rs`）を実装済み。ホスト→ページ通知は `evaluate_javascript`（`presentation`・`state-updated`・`file-drop`・`theme-changed`・`power-event`・`global-hotkey`・`capture-saved`・`second-instance`・`open-url`・`notification-clicked` 実装済み） | 機能は実装可（実装済み） |
| **preload の分離ワールド**（`contextBridge` 級の分離） | **Servo 0.8.0 の公開 API に存在しない**。`UserScript` はページのメインワールドで実行される | 同一 realm 上の多層防御として、① 起動毎 128-bit セッショントークン（クロージャ内秘匿）＋ネイティブ `prompt` 捕捉＋`Object.freeze` / non-configurable 化、② `UserContentManager::add_stylesheet(Rc<UserStyleSheet>)` によるインライン `<style>` 排除と厳格 CSP（`script-src 'self' qv://app; style-src 'self' qv://app; object-src 'none'`）、③ `policy.rs` によるアプリ別・子ウィンドウ減衰付き `Capabilities` ACL 強制を実装済み。完全な JS コンパートメント分離は引き続きエンジン変更枠（§4 参照） | 強化実装済み（完全分離はエンジン変更） |
| `dialog`（open/save/folder/message box） | `<input type=file>` 等は delegate `EmbedderControl::FilePicker`、ページ発の `SimpleDialog::{Alert, Confirm, Prompt}`、`ColorPicker`、`SelectElement` がホストへ来る | Win32 `MessageBoxW` / `GetOpenFileNameW` / `IFileOpenDialog`（フォルダ選択）/ `GetSaveFileNameW` / `ChooseColorW` / `TrackPopupMenuEx` を実装済み。JS 向けにも `QuantumView.dialog.{openFile, selectFolder, saveTextFile, saveBase64File}`（`*Async` 版あり、`FileDialogs` 権限ゲート付き）を公開 | 実装可（実装済み） |
| `Menu` / コンテキストメニュー | イベントとデータは delegate `EmbedderControl::ContextMenu`（リンク/画像/編集可否つき）で届く。描画は埋め込み側。winit にメニュー API なし | Win32 `CreatePopupMenu` / `TrackPopupMenuEx` で `ContextMenu`・`SelectElement`・トレイメニューを実装済み | 実装可（実装済み） |
| `Tray` | Servo / winit に存在しない | `tray.rs` で専用スレッドの隠しトップレベルツールウィンドウ＋Win32 `Shell_NotifyIconW`（`--tray` で有効化、右クリックメニュー・表示切替・終了、Drop 時に `NIM_DELETE`）。トレイ有効時のみ「閉じる＝隠す」と `qv-window:hide` を許可 | 実装可（実装済み） |
| `clipboard` | ページ側は `ClipboardDelegate`（テキストのみ）が一級市民で、`clipboard` feature の既定実装（arboard）が有効済み。`navigator.clipboard` も同 delegate 経由 | `win_platform.rs` で Win32 `OpenClipboard` による `CF_UNICODETEXT`（テキスト読み書き）、`"HTML Format"`（`CF_HTML` ヘッダ構築・Fragment 抽出）、`CF_DIB`（BGRA DIB ↔ RGBA / ストア PNG / BMP 変換）および `EmptyClipboard` を実装し、`QuantumView.clipboard.{readText, writeText, readHtml, writeHtml, readImage, writeImage, clear}`（`Async` 版・`ClipboardRead`/`ClipboardWrite` 権限ゲート付き）として公開済み | 実装可（実装済み） |
| `globalShortcut` | Servo / winit に存在しない（winit のキーイベントはフォーカス時のみ） | `--hotkey "Ctrl+Alt+Q"` 等で `RegisterHotKey` / `UnregisterHotKey`＋`WM_HOTKEY`（`tray.rs`＋`win_platform::parse_hotkey`）を実装済み。押下時にウィンドウ表示切替と `global-hotkey` イベント通知 | 実装可（実装済み） |
| `Notification` | ページ側 Notification API は pref ゲート。表示自体は delegate `show_notification` でホストへ委譲 | `Delegate::show_notification` および `QuantumView.notify({ title, body })` を実装済み。常駐トレイ有効時はメインアイコン（`u_id = 1`）、`--tray` なしでも一時バルーンアイコン（`u_id = 2`、15 秒タイマーおよび `NIN_BALLOONTIMEOUT`/`NIN_BALLOONHIDE` で自動消去）により OS 通知を表示し、クリック時は `NIN_BALLOONUSERCLICK` → `notification-clicked` イベントをページへ配信 | 実装可（実装済み） |
| `shell`（openExternal 等） | 外部リンクは `request_navigation` 拒否→ブリッジ `window.openExternal` 経由で引き渡し | `ShellExecuteW`（`embedder_ui.rs`）。**`bridge::allows_external_url` で `https://`（ホスト必須）/ `mailto:`（`@` 必須・制御/空白/引用符/バックスラッシュ禁止）のみ許可し、`file:` / `http:` / UNC / 実行ファイルを拒否**（実装済み） | 実装可（実装済み） |
| `screen` | ページ内 `window.screen` 等は delegate `screen_geometry()` が供給 | winit `current_monitor` / `primary_monitor` と `outer_position` / `outer_size` から `ScreenGeometry` を算出して供給（実装済み） | 実装可（実装済み） |
| `powerMonitor` | 存在しない | `tray.rs` の隠しトップレベルウィンドウで `WM_POWERBROADCAST`（`suspend` / `resume` / `power-status`）を受信し、ページへ `power-event` 通知（実装済み） | 実装可（実装済み） |
| `nativeImage` | favicon・スクリーンショットで画像が渡されるのみ | アイコン用途に限定（PNG→`winit::Icon::from_rgba`、トレイは `HICON`） | 実装可（範囲限定） |
| `systemPreferences` / `nativeTheme` | `WebView::notify_theme_change(Theme)` で `prefers-color-scheme` を反映可能 | `RegGetValueW`（`AppsUseLightTheme`）＋`WM_SETTINGCHANGE` 監視→`notify_theme_change`＋Win11 DWM ダークタイトルバー＋`theme-changed` 通知＋`window.getTheme()` を実装済み | 実装可（実装済み） |
| `session`（サイトデータ・Cookie・HTTP キャッシュ） | `Servo::site_data_manager()` の `SiteDataManager`（`site_data(StorageType::all())`、`clear_site_data`、`clear_cookies`）および `Servo::network_manager().clear_cache()` | `QuantumView.session.{listSiteData, clearSiteData, clearCookies, clearCache}`（および `Async` 版、`SiteData` 権限ゲート付き）としてホスト配線を実装済み | 実装可（実装済み） |
| `session`（権限） | delegate `request_permission`（地磁気・通知・カメラ等、`PermissionFeature`） | `Delegate::request_permission` でログ出力の上すべて既定拒否（実装済み） | 実装可（実装済み） |
| `net`（メインプロセス HTTP） | ホスト向け HTTP API なし。ページ内 `fetch()` は Servo ネットスタック | ホストは Rust の HTTP を直接使う（既存方針 rustls）。WebView の Cookie とは自動共有されない | 実装可（意味論は異なる） |
| `autoUpdater` | 存在しない | ポータブル配布前提の自前最小アップデータ（マニフェスト取得→署名/ハッシュ検証→入替→再起動）は可能。Squirrel 互換は作らない。**更新経路は RCE 経路なので検証必須** | 自前実装は可、互換は対象外 |
| `crashReporter` | パイプライン panic は delegate `notify_crashed(reason, backtrace)`、ホスト側 panic は `std::panic::set_hook`（`!servo::should_panic_hook_suppress_termination()` フィルタ付き）で捕捉可能 | `Delegate::notify_crashed`（`source: "webview-crashed"`）およびホスト panic hook（`source: "rust-panic"`）の双方で stderr ログ出力に加え、`--crash-dir`（または既定ディレクトリ）へ構造化 JSON レポート（`.crash.json`）を書き出す `write_crash_report` を実装済み | 実装可（構造化 JSON レポート実装済み） |
| `safeStorage`（DPAPI） | Web 側に相当なし | Win32 `CryptProtectData` / `CryptUnprotectData`（`win_platform.rs`）＋`QuantumView.safeStorage.{encrypt, decrypt}`（UTF-8 ↔ base64、1:1 Electron Windows 意味論）を実装済み。同ユーザーの他プロセスからは守れない点は Electron と同じ | 実装可（実装済み） |

## 2. Chromium / Blink 限定機能の扱い

分類: **動作** / **部分**（pref ゲート・不完全・要実機検証） / **ホスト代替** / **不可**。

| 機能 | Servo 0.8.0 での状態 | 分類 |
| --- | --- | --- |
| Service Worker | pref `dom_serviceworker_enabled`、WIP。Push / Background Sync はなし | 部分 / Push は不可 |
| WebGPU | pref `dom_webgpu_enabled`、wgpu 実装で収束中 | 部分 |
| WebRTC / getUserMedia | pref `dom_webrtc_enabled` 等、基礎のみ。`getDisplayMedia`・DataChannel の完全性は未確認 | 部分 |
| WebUSB / WebBluetooth / WebSerial / WebHID | Mozilla が標準化に否定的な Fugu API 群。Servo に USB/Serial/HID の pref 自体がない | エンジンでは不可。**ホスト代替が本命**: `btleplug` / `serialport` / `rusb` / `hidapi` をホストに置き QuantumView 独自 API として公開（Web API そのままのコードは動かない） |
| File System Access（`showOpenFilePicker` 等） | ピッカーは存在しない。OPFS も証跡なし | **ホスト代替実装済み**: `QuantumView.dialog.{openFile, selectFolder, saveTextFile, saveBase64File}`＋ケーパビリティトークン経由 `QuantumView.files.{readText, readBase64}`（最大 16 MiB、任意パス読取不可）。OPFS 前提ライブラリはホスト側 DB で代替 |
| Notifications / Push | Notification API は pref `dom_notification_enabled`、表示はホスト委譲。Push は不可 | **ホスト代替実装済み**（`show_notification`＋`QuantumView.notify`→トレイまたは一時トレイアイコン `NIF_INFO` バルーン＋クリック通知 `notification-clicked`） |
| Async Clipboard | pref `dom_async_clipboard_enabled`。テキストは実用、リッチ形式は要検証 | **ホスト代替実装済み**（`QuantumView.clipboard` でテキスト / `CF_HTML` / `CF_DIB` 画像 PNG・BMP をサポート） |
| ドラッグ＆ドロップ / DataTransfer | 埋め込み API に DnD が存在しない（delegate・入力イベント・script 側いずれもなし） | **ホスト代替実装済み**: winit `HoveredFile` / `HoveredFileCancelled` / `DroppedFile` をホストでバッチ集約し、ケーパビリティトークン発行＋`file-drop` 通知＋合成 DOM `CustomEvent("qv:file-drop")` 配信＋`QuantumView.files.{readText, readBase64}`（`workspace.js` にドロップ取込を配線済み） |
| IndexedDB | pref `dom_indexeddb_enabled`、実験扱い | 部分（使うクエリパターンを実機で適合確認すること） |
| localStorage / sessionStorage / Cookie | 動作。`SiteDataManager`＋`NetworkManager` 経由で `QuantumView.session` から一覧・消去も可能 | 動作（ホスト管理 API 実装済み） |
| WebAssembly（core / SIMD / GC） | SpiderMonkey 上で動作 | 動作 |
| Wasm スレッド / SharedArrayBuffer | cross-origin isolation 前提。Servo での SAB 可用性は未確認 | 部分（未確認） |
| WebGL 1 / 2 | WebGL1 は既定有効。WebGL2 は pref `dom_webgl2_enabled` | 動作 / 部分 |
| WebAudio | `servo-media` で主要用途は動作、ノード単位の欠落あり。MediaRecorder・Web Speech は証跡なし | 部分 / ホスト代替 |
| Payment Request / WebAuthn | Payment はなし。Credential Management は pref のみ | 不可 / 部分（未確認） |
| CSS（Stylo = Firefox と同エンジン） | Grid 既定有効。Container Queries・`backdrop-filter` は pref ゲート、View Transitions・Anchor Positioning・Scroll-driven Animations は証跡なし。Popover API は未実装 | **Popover API ポリフィル実装済み**（`bridge.rs`＋`UserStyleSheet` 注入: `[popover]` 非表示・`showPopover/hidePopover/togglePopover`・`popovertarget`・ライトディスミス・`beforetoggle/toggle`、インライン `<style>` 不使用で厳格 CSP 適合） |
| HTML フォーム部品 | ピッカー類は埋め込み側 delegate 実装が前提。`<dialog>` は部分実装、`<progress>` は 0.7.0 で新規 | ホスト側コントロール実装済み（`embedder_ui.rs` 全 6 種） |
| Chrome 拡張機能（`chrome.*`、MV3） | 拡張システム自体がない | 不可（特定の拡張の振る舞いだけホスト機能として再実装するなら可） |
| DevTools プロトコル | CDP は不可。Firefox RDP サーバ＋Firefox クライアントが代替。自動化は W3C WebDriver | 部分（`--devtools` / `--devtools-addr` で pref 配線済み、要実機確認） |
| EME / Widevine | CDM はライセンス済みバイナリであり Servo 埋め込み側に提供されない | 不可（保護付き配信は対象外と明示する） |
| コーデック | Chromium のような同梱はなく **GStreamer プラグイン構成に依存**。H.264/AAC はプラグイン＋特許ライセンスが配布側の責任。MSE 適合（DASH/HLS プレイヤ）は要実機検証。Windows のメディア経路は歴史的に最も弱い | 部分（パッケージング依存、要検証） |
| 印刷 / PDF | §1 の通り印刷系 API なし | **単一ページラスタ PDF / HTML / PNG 出力実装済み**（`window.print()` → `exportPdf`、`exportHtml`、`capturePng`） |

## 3. SpiderMonkey と V8 の差異（ページ JS）

- 標準化された言語機能（ES2024/25 相当、モジュール、Top-level await、JSON/CSS モジュール等）は動作する。Electron レンダラの標準 JS/TS はそのまま動くのが原則。
- 実務で踏みやすい差異: `Error.stack` の書式（スタックをパースするコード・エラー収集ライブラリが壊れる）、V8/Node 由来の非標準 API（`global`、`process`、`Buffer`、`require`、`Error.captureStackTrace` 等は存在しない）、Intl のロケールデータ（ICU4X と V8-ICU の端数差異）、JIT の性能プロファイル（Ion/Warp と TurboFan でホットパスの速度が数倍単位で入れ替わりうるため実測前提）。

## 4. セキュリティモデル（Electron との最大の差異）

Electron の `contextBridge` は preload を分離ワールドで実行し、ページからブリッジ内部を触れないことを防御の前提にできる。**Servo 0.8.0 の `UserScript` はページのメインワールドで動くため、同一 JS realm の制約自体は残る**。

QuantumView は同一 realm 上の多層防御＋ホスト境界防御を組み合わせる:

1. 特権 WebView にはローカル同梱文書のみを載せる（遷移ガードでローカル `file` / `qv://app/` / `about:blank`、および明示指定時の `--dev-url http://127.0.0.1:<port>` ループバック開発サーバ以外を拒否）。
2. `qv://app/` の HTML 応答には既定で `Content-Security-Policy`（`default-src 'self' qv://app; script-src 'self' qv://app; style-src 'self' qv://app; ...; object-src 'none'`）と `X-Content-Type-Options: nosniff` を付与し、Popover ポリフィル CSS は `UserContentManager::add_stylesheet(Rc<UserStyleSheet>)` で注入することで `'unsafe-inline'` スタイルも排除する。
3. プリロード起動時に 128-bit の起動毎セッショントークンをクロージャ内のみに秘匿し、ネイティブ `window.prompt` を捕捉した上で `window.QuantumView`（全サブ名前空間を含む）を再帰的に `Object.freeze`＋non-configurable 定義する。未認証の生の `window.prompt("qv-window:...")` / `window.prompt("qv-async:...")` はホスト側で拒否する。
4. ホストは [policy.rs](../crates/quantumview-host/src/policy.rs) の宣言的 `Capabilities` ACL（`ExternalLinks`, `FileDialogs`, `FileDrop`, `SafeStorage`, `Notifications`, `WindowSizing`, `OpacityControl`, `Capture`, `ClipboardRead`, `ClipboardWrite`, `Autostart`, `ProtocolClient`, `SiteData`, `ChildWindows`, `AllowedAppCommands`）により、アプリモード毎・子ウィンドウ減衰（`attenuate_for_child()`）毎に許可されたコマンドと delegate 操作のみを通す。ファイル読取は OS ドロップ・ネイティブファイルピッカーで発行した不透明なケーパビリティトークン経由のみ（最大 16 MiB、ディレクトリパス非開示）に限定する。
5. Node がレンダラに存在しないこと自体が Electron 比の安全側の差分であり、これを崩す汎用コマンドを足さない。

## 5. ハードな除外事項（製品判断として明示）

- Node.js ランタイム互換・npm ネイティブアドオンの直接実行
- Chrome 拡張機能、CDP ツール群（Puppeteer/Playwright-CDP、`webContents.debugger`）
- Widevine 等の保護付きストリーミング再生
- Chromium フォールバック / マルチエンジン切替

## 出典

- Servo 0.8.0 ソース（GitHub タグ `v0.8.0`）: `components/servo/{lib.rs, webview.rs, webview_delegate.rs, servo.rs, user_content_manager.rs}`、`components/net/protocols/{mod.rs, data.rs}`、`components/shared/net/response.rs`、`components/shared/embedder/`
- Servo Book（実験的機能・DevTools）: https://book.servo.org/design-documentation/experimental-features.html / https://book.servo.org/contributing/devtools.html
- Electron 公式ドキュメント（app / BrowserWindow / webContents / session / protocol / safeStorage / セキュリティ）: https://www.electronjs.org/docs/latest/
- winit 0.30.13 ソース（`window.rs`、`platform/windows.rs`）
