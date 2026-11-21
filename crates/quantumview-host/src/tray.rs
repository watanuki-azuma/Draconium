//! System tray icon + hidden OS message window foundation (Milestone A / C +
//! engine-gap OS integration).
//!
//! Runs a dedicated hidden Win32 top-level tool window on a background thread
//! so OS events never interfere with winit's main window loop. Using a hidden
//! unshown top-level window (instead of `HWND_MESSAGE`) allows receiving
//! system broadcasts (`WM_SETTINGCHANGE` for Windows theme changes and
//! `WM_POWERBROADCAST` for suspend/resume) in addition to `WM_TRAYICON`,
//! `WM_HOTKEY`, and `WM_COPYDATA` (for single-instance argument and deep-link
//! forwarding).
//!
//! When `--tray` is omitted (`show_tray_icon == false`), `show_notification`
//! uses an ephemeral notification icon (`u_id = 2`) on the hidden helper
//! window and automatically deletes it when the balloon closes or after a
//! 15-second timeout.
//!
//! Pure-logic helpers live outside `#[cfg(target_os = "windows")]` so they are
//! unit-tested on any OS via `rustc --edition 2024 --test`.

/// High-level OS/tray action forwarded to the host event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleVisible,
    ShowChips,
    ShowFlyout,
    Quit,
    /// Global hotkey registered via `--hotkey` was pressed.
    GlobalHotkey,
    /// Windows system settings changed (e.g. `AppsUseLightTheme` dark/light toggle).
    ThemeChanged,
    /// Windows power broadcast (`"suspend"`, `"resume"`, `"power-status"`).
    PowerEvent(&'static str),
    /// User clicked a desktop notification balloon.
    NotificationClicked,
    /// A second instance forwarded its CLI arguments / deep-link URI via `WM_COPYDATA`.
    SecondInstance(String),
}

/// Configuration for the background Win32 tray & OS-event monitor window.
#[derive(Clone, Debug)]
pub struct TrayConfig {
    pub tooltip: String,
    pub presentations: bool,
    /// Whether to add a persistent notification-area icon (`--tray`).
    pub show_tray_icon: bool,
    /// Optional Win32 `(modifiers, vk)` global hotkey to register.
    pub hotkey: Option<(u32, u32)>,
    /// Per-mode instance tag scoping the hidden OS event window class and title
    /// (`"QuantumViewOsEventWindow.<tag>"`) so concurrent modes do not collide
    /// under `FindWindowExW`.
    pub instance_window_tag: String,
}

const CMD_TOGGLE: u32 = 1001;
const CMD_CHIPS: u32 = 1002;
const CMD_FLYOUT: u32 = 1003;
const CMD_QUIT: u32 = 1004;

const PBT_APMSUSPEND: usize = 0x0004;
const PBT_APMPOWERSTATUSCHANGE: usize = 0x000A;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

/// Magic `dwData` tag identifying QuantumView `WM_COPYDATA` packets (`"QVIP"`).
pub const QV_COPYDATA_MAGIC: usize = 0x5156_4950;
/// Maximum byte length accepted for a `WM_COPYDATA` second-instance payload.
pub const MAX_COPYDATA_BYTES: usize = 65_536;

/// Map a popup menu command ID back to a [`TrayAction`].
pub fn action_from_menu_id(id: u32) -> Option<TrayAction> {
    match id {
        CMD_TOGGLE => Some(TrayAction::ToggleVisible),
        CMD_CHIPS => Some(TrayAction::ShowChips),
        CMD_FLYOUT => Some(TrayAction::ShowFlyout),
        CMD_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

/// Map a `WM_POWERBROADCAST` `wParam` into a normalized event name.
pub fn power_event_from_wparam(wparam: usize) -> Option<&'static str> {
    match wparam {
        PBT_APMSUSPEND => Some("suspend"),
        PBT_APMRESUMEAUTOMATIC => Some("resume"),
        PBT_APMPOWERSTATUSCHANGE => Some("power-status"),
        _ => None,
    }
}

/// Compute the scoped Win32 window class and title for a given instance tag.
pub fn os_event_window_name(tag: &str) -> String {
    let cleaned: String = tag
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        .take(48)
        .collect();
    let suffix = if cleaned.is_empty() {
        "Default"
    } else {
        cleaned.as_str()
    };
    format!("QuantumViewOsEventWindow.{suffix}")
}

/// Validate a raw `WM_COPYDATA` byte slice (must be non-empty, `<= MAX_COPYDATA_BYTES`,
/// valid UTF-8 JSON object without NUL bytes).
pub fn decode_copydata_payload(magic: usize, raw: &[u8]) -> Option<String> {
    if magic != QV_COPYDATA_MAGIC || raw.is_empty() || raw.len() > MAX_COPYDATA_BYTES {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?.trim();
    if !text.starts_with('{') || !text.ends_with('}') || text.contains('\0') {
        return None;
    }
    Some(text.to_owned())
}

/// Encode text into a fixed-length NUL-terminated UTF-16 array (used for
/// `NOTIFYICONDATAW.szTip` [128], `szInfoTitle` [64], and `szInfo` [256])
/// without ever splitting a UTF-16 surrogate pair.
pub fn encode_wide_fixed<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0u16; N];
    if N == 0 {
        return out;
    }
    let mut len = 0usize;
    for ch in text.chars() {
        if ch == '\0' {
            continue;
        }
        let mut buf = [0u16; 2];
        let encoded = ch.encode_utf16(&mut buf);
        if len + encoded.len() >= N {
            break;
        }
        out[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    out[len] = 0;
    out
}

/// Encode a tray tooltip into the fixed 128-`u16` buffer of `NOTIFYICONDATAW`
/// (`szTip`).
pub fn encode_tray_tip(text: &str) -> [u16; 128] {
    encode_wide_fixed::<128>(text)
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    use super::{
        CMD_CHIPS, CMD_FLYOUT, CMD_QUIT, CMD_TOGGLE, MAX_COPYDATA_BYTES, TrayAction, TrayConfig,
        action_from_menu_id, decode_copydata_payload, encode_tray_tip, encode_wide_fixed,
        os_event_window_name, power_event_from_wparam,
    };

    type Hwnd = isize;
    type Hinstance = isize;
    type Hicon = isize;
    type Hcursor = isize;
    type Hbrush = isize;
    type Hmenu = *mut c_void;

    const WM_NULL: u32 = 0x0000;
    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const WM_COPYDATA: u32 = 0x004A;
    const WM_CONTEXTMENU: u32 = 0x007B;
    const WM_TIMER: u32 = 0x0113;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_POWERBROADCAST: u32 = 0x0218;
    const WM_HOTKEY: u32 = 0x0312;
    const WM_USER: u32 = 0x0400;
    const WM_TRAYICON: u32 = WM_USER + 1;
    const WM_EPHEMERAL_BALLOON: u32 = WM_USER + 2;
    const WM_SHOW_EPHEMERAL_NOTIFY: u32 = WM_USER + 20;

    const NIN_BALLOONHIDE: u32 = WM_USER + 3;
    const NIN_BALLOONTIMEOUT: u32 = WM_USER + 4;
    const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

    const PRIMARY_NID_ID: u32 = 1;
    const EPHEMERAL_NID_ID: u32 = 2;
    const TIMER_EPHEMERAL_CLEANUP: usize = 10;
    const EPHEMERAL_TIMEOUT_MS: u32 = 15_000;

    const HOTKEY_ID: i32 = 1;
    const GWLP_USERDATA: i32 = -21;
    const IDI_APPLICATION: *const u16 = 32512usize as *const u16;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_POPUP: u32 = 0x8000_0000;

    const NIM_ADD: u32 = 0x0000_0000;
    const NIM_MODIFY: u32 = 0x0000_0001;
    const NIM_DELETE: u32 = 0x0000_0002;
    const NIF_MESSAGE: u32 = 0x0000_0001;
    const NIF_ICON: u32 = 0x0000_0002;
    const NIF_TIP: u32 = 0x0000_0004;
    const NIF_INFO: u32 = 0x0000_0010;
    const NIIF_INFO: u32 = 0x0000_0001;

    const MF_STRING: u32 = 0x0000_0000;
    const MF_SEPARATOR: u32 = 0x0000_0800;
    const TPM_LEFTALIGN: u32 = 0x0000_0000;
    const TPM_TOPALIGN: u32 = 0x0000_0000;
    const TPM_RIGHTBUTTON: u32 = 0x0000_0002;
    const TPM_NONOTIFY: u32 = 0x0000_0080;
    const TPM_RETURNCMD: u32 = 0x0000_0100;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CopyDataStruct {
        dw_data: usize,
        cb_data: u32,
        lp_data: *const c_void,
    }

    #[repr(C)]
    struct WndClassExW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: Hinstance,
        h_icon: Hicon,
        h_cursor: Hcursor,
        hbr_background: Hbrush,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
        h_icon_sm: Hicon,
    }

    #[repr(C)]
    struct Msg {
        hwnd: Hwnd,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: Point,
    }

    #[repr(C)]
    #[cfg_attr(target_arch = "x86", repr(packed(1)))]
    struct NotifyIconDataW {
        cb_size: u32,
        h_wnd: Hwnd,
        u_id: u32,
        u_flags: u32,
        u_callback_message: u32,
        h_icon: Hicon,
        sz_tip: [u16; 128],
        dw_state: u32,
        dw_state_mask: u32,
        sz_info: [u16; 256],
        u_timeout_or_version: u32,
        sz_info_title: [u16; 64],
        dw_info_flags: u32,
        guid_item: Guid,
        h_balloon_icon: Hicon,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> Hinstance;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(wc: *const WndClassExW) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Hwnd,
            menu: Hmenu,
            instance: Hinstance,
            param: *mut c_void,
        ) -> Hwnd;
        fn DestroyWindow(hwnd: Hwnd) -> i32;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn PostQuitMessage(exit_code: i32);
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, new_long: isize) -> isize;
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        fn LoadIconW(instance: Hinstance, icon_name: *const u16) -> Hicon;
        fn CreatePopupMenu() -> Hmenu;
        fn AppendMenuW(hmenu: Hmenu, flags: u32, id_new_item: usize, new_item: *const u16) -> i32;
        fn TrackPopupMenuEx(
            hmenu: Hmenu,
            flags: u32,
            x: i32,
            y: i32,
            hwnd: Hwnd,
            lptpm: *const c_void,
        ) -> i32;
        fn DestroyMenu(hmenu: Hmenu) -> i32;
        fn GetCursorPos(pt: *mut Point) -> i32;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn RegisterHotKey(hwnd: Hwnd, id: i32, modifiers: u32, vk: u32) -> i32;
        fn UnregisterHotKey(hwnd: Hwnd, id: i32) -> i32;
        fn SetTimer(hwnd: Hwnd, id_event: usize, elapse: u32, timer_func: *const c_void) -> usize;
        fn KillTimer(hwnd: Hwnd, id_event: usize) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *const NotifyIconDataW) -> i32;
    }

    struct TrayContext {
        tooltip: String,
        presentations: bool,
        has_tray_icon: bool,
        has_hotkey: bool,
        on_action: Box<dyn Fn(TrayAction) + Send + 'static>,
    }

    fn wide_nul(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    fn base_nid(hwnd: Hwnd) -> NotifyIconDataW {
        NotifyIconDataW {
            cb_size: std::mem::size_of::<NotifyIconDataW>() as u32,
            h_wnd: hwnd,
            u_id: PRIMARY_NID_ID,
            u_flags: 0,
            u_callback_message: WM_TRAYICON,
            h_icon: 0,
            sz_tip: [0u16; 128],
            dw_state: 0,
            dw_state_mask: 0,
            sz_info: [0u16; 256],
            u_timeout_or_version: 0,
            sz_info_title: [0u16; 64],
            dw_info_flags: 0,
            guid_item: Guid {
                data1: 0,
                data2: 0,
                data3: 0,
                data4: [0; 8],
            },
            h_balloon_icon: 0,
        }
    }

    fn ephemeral_nid(hwnd: Hwnd) -> NotifyIconDataW {
        let mut nid = base_nid(hwnd);
        nid.u_id = EPHEMERAL_NID_ID;
        nid.u_callback_message = WM_EPHEMERAL_BALLOON;
        nid
    }

    unsafe fn cleanup_ephemeral_balloon(hwnd: Hwnd) {
        unsafe {
            KillTimer(hwnd, TIMER_EPHEMERAL_CLEANUP);
            let nid = ephemeral_nid(hwnd);
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    unsafe fn show_tray_menu(hwnd: Hwnd, ctx: &TrayContext) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let toggle_w = wide_nul("表示 / 非表示");
            AppendMenuW(menu, MF_STRING, CMD_TOGGLE as usize, toggle_w.as_ptr());
            if ctx.presentations {
                let chips_w = wide_nul("チップス表示");
                let flyout_w = wide_nul("詳細パネル");
                AppendMenuW(menu, MF_STRING, CMD_CHIPS as usize, chips_w.as_ptr());
                AppendMenuW(menu, MF_STRING, CMD_FLYOUT as usize, flyout_w.as_ptr());
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            let quit_w = wide_nul("終了");
            AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, quit_w.as_ptr());

            let mut pt = Point { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenuEx(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                hwnd,
                ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            if cmd > 0
                && let Some(action) = action_from_menu_id(cmd as u32)
            {
                (ctx.on_action)(action);
            }
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        unsafe {
            match msg {
                WM_TRAYICON => {
                    let event = (lparam as u32) & 0xffff;
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null() {
                        let ctx = &*ptr;
                        match event {
                            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                                (ctx.on_action)(TrayAction::ToggleVisible);
                            }
                            WM_RBUTTONUP | WM_CONTEXTMENU => {
                                show_tray_menu(hwnd, ctx);
                            }
                            NIN_BALLOONUSERCLICK => {
                                (ctx.on_action)(TrayAction::NotificationClicked);
                            }
                            _ => {}
                        }
                    }
                    0
                }
                WM_EPHEMERAL_BALLOON => {
                    let event = (lparam as u32) & 0xffff;
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    match event {
                        NIN_BALLOONUSERCLICK | WM_LBUTTONUP => {
                            cleanup_ephemeral_balloon(hwnd);
                            if !ptr.is_null() {
                                ((*ptr).on_action)(TrayAction::NotificationClicked);
                            }
                        }
                        NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT => {
                            cleanup_ephemeral_balloon(hwnd);
                        }
                        _ => {}
                    }
                    0
                }
                WM_SHOW_EPHEMERAL_NOTIFY => {
                    if lparam != 0 {
                        let boxed = Box::from_raw(lparam as *mut (String, String));
                        let (title, body) = *boxed;
                        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                        let tip = if !ptr.is_null() {
                            (*ptr).tooltip.as_str()
                        } else {
                            "QuantumView"
                        };
                        cleanup_ephemeral_balloon(hwnd);
                        let mut nid = ephemeral_nid(hwnd);
                        nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_INFO;
                        nid.h_icon = LoadIconW(0, IDI_APPLICATION);
                        nid.sz_tip = encode_tray_tip(tip);
                        nid.sz_info_title = encode_wide_fixed::<64>(&title);
                        nid.sz_info = encode_wide_fixed::<256>(&body);
                        nid.dw_info_flags = NIIF_INFO;
                        if Shell_NotifyIconW(NIM_ADD, &nid) != 0 {
                            SetTimer(
                                hwnd,
                                TIMER_EPHEMERAL_CLEANUP,
                                EPHEMERAL_TIMEOUT_MS,
                                ptr::null(),
                            );
                        }
                    }
                    0
                }
                WM_TIMER => {
                    if wparam == TIMER_EPHEMERAL_CLEANUP {
                        cleanup_ephemeral_balloon(hwnd);
                    }
                    0
                }
                WM_COPYDATA => {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null() && lparam != 0 {
                        let cds = &*(lparam as *const CopyDataStruct);
                        if !cds.lp_data.is_null()
                            && cds.cb_data > 0
                            && (cds.cb_data as usize) <= MAX_COPYDATA_BYTES
                        {
                            let slice = std::slice::from_raw_parts(
                                cds.lp_data.cast::<u8>(),
                                cds.cb_data as usize,
                            );
                            if let Some(payload) = decode_copydata_payload(cds.dw_data, slice) {
                                ((*ptr).on_action)(TrayAction::SecondInstance(payload));
                                return 1;
                            }
                        }
                    }
                    0
                }
                WM_HOTKEY => {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null() && wparam == HOTKEY_ID as usize {
                        ((*ptr).on_action)(TrayAction::GlobalHotkey);
                    }
                    0
                }
                WM_SETTINGCHANGE => {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null() {
                        ((*ptr).on_action)(TrayAction::ThemeChanged);
                    }
                    0
                }
                WM_POWERBROADCAST => {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null()
                        && let Some(kind) = power_event_from_wparam(wparam)
                    {
                        ((*ptr).on_action)(TrayAction::PowerEvent(kind));
                    }
                    1
                }
                WM_CLOSE => {
                    cleanup_ephemeral_balloon(hwnd);
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayContext;
                    if !ptr.is_null() {
                        let ctx = &*ptr;
                        if ctx.has_hotkey {
                            UnregisterHotKey(hwnd, HOTKEY_ID);
                        }
                        if ctx.has_tray_icon {
                            let nid = base_nid(hwnd);
                            Shell_NotifyIconW(NIM_DELETE, &nid);
                        }
                    }
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    pub struct TrayHandle {
        hwnd: Hwnd,
        has_tray_icon: bool,
        thread: Option<JoinHandle<()>>,
    }

    impl TrayHandle {
        pub fn start(
            config: TrayConfig,
            on_action: impl Fn(TrayAction) + Send + 'static,
        ) -> Result<Self, String> {
            let (tx, rx) = mpsc::channel::<Result<(Hwnd, bool), String>>();
            let thread = thread::Builder::new()
                .name("quantumview-tray".to_owned())
                .spawn(move || unsafe {
                    let hinstance = GetModuleHandleW(ptr::null());
                    let scoped_name = os_event_window_name(&config.instance_window_tag);
                    let class_name = wide_nul(&scoped_name);
                    let wc = WndClassExW {
                        cb_size: std::mem::size_of::<WndClassExW>() as u32,
                        style: 0,
                        lpfn_wnd_proc: Some(wnd_proc),
                        cb_cls_extra: 0,
                        cb_wnd_extra: 0,
                        h_instance: hinstance,
                        h_icon: 0,
                        h_cursor: 0,
                        hbr_background: 0,
                        lpsz_menu_name: ptr::null(),
                        lpsz_class_name: class_name.as_ptr(),
                        h_icon_sm: 0,
                    };
                    let _ = RegisterClassExW(&wc);
                    let hwnd = CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        class_name.as_ptr(),
                        class_name.as_ptr(),
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        0,
                        ptr::null_mut(),
                        hinstance,
                        ptr::null_mut(),
                    );
                    if hwnd == 0 {
                        let _ = tx.send(Err(
                            "隠し OS イベントウィンドウの作成に失敗しました".to_owned()
                        ));
                        return;
                    }

                    let mut has_tray_icon = false;
                    if config.show_tray_icon {
                        let mut nid = base_nid(hwnd);
                        nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
                        nid.h_icon = LoadIconW(0, IDI_APPLICATION);
                        nid.sz_tip = encode_tray_tip(&config.tooltip);
                        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
                            eprintln!(
                                "[tray] タスクトレイアイコンの登録をスキップしました (Shell_NotifyIconW(NIM_ADD) failed); OS イベント監視は継続します"
                            );
                        } else {
                            has_tray_icon = true;
                        }
                    }

                    let mut has_hotkey = false;
                    if let Some((mods, vk)) = config.hotkey {
                        if RegisterHotKey(hwnd, HOTKEY_ID, mods, vk) != 0 {
                            has_hotkey = true;
                        } else {
                            eprintln!(
                                "[hotkey] グローバルホットキーの登録に失敗しました (mods=0x{mods:x}, vk=0x{vk:x})"
                            );
                        }
                    }

                    let ctx = Box::new(TrayContext {
                        tooltip: config.tooltip,
                        presentations: config.presentations,
                        has_tray_icon,
                        has_hotkey,
                        on_action: Box::new(on_action),
                    });
                    let ctx_ptr = Box::into_raw(ctx);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr as isize);

                    let _ = tx.send(Ok((hwnd, has_tray_icon)));

                    let mut msg = Msg {
                        hwnd: 0,
                        message: 0,
                        w_param: 0,
                        l_param: 0,
                        time: 0,
                        pt: Point { x: 0, y: 0 },
                    };
                    while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    cleanup_ephemeral_balloon(hwnd);
                    if has_hotkey {
                        UnregisterHotKey(hwnd, HOTKEY_ID);
                    }
                    if has_tray_icon {
                        let cleanup = base_nid(hwnd);
                        Shell_NotifyIconW(NIM_DELETE, &cleanup);
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(ctx_ptr));
                })
                .map_err(|err| format!("OS イベントスレッド起動失敗: {err}"))?;

            match rx.recv() {
                Ok(Ok((hwnd, has_tray_icon))) => Ok(Self {
                    hwnd,
                    has_tray_icon,
                    thread: Some(thread),
                }),
                Ok(Err(err)) => {
                    let _ = thread.join();
                    Err(err)
                }
                Err(_) => {
                    let _ = thread.join();
                    Err("OS イベント初期化スレッドが応答せず終了しました".to_owned())
                }
            }
        }

        pub fn has_tray_icon(&self) -> bool {
            self.has_tray_icon
        }

        /// Display a Windows notification balloon/toast.
        ///
        /// When `--tray` is active, modifies the persistent tray icon (`u_id = 1`).
        /// When `--tray` is omitted, posts an ephemeral notification request
        /// (`u_id = 2`) to the hidden OS event window that auto-cleans on
        /// balloon dismissal or a 15-second timer.
        pub fn show_notification(&self, title: &str, body: &str) -> bool {
            if self.hwnd == 0 {
                return false;
            }
            if self.has_tray_icon {
                let mut nid = base_nid(self.hwnd);
                nid.u_flags = NIF_INFO;
                nid.sz_info_title = encode_wide_fixed::<64>(title);
                nid.sz_info = encode_wide_fixed::<256>(body);
                nid.dw_info_flags = NIIF_INFO;
                // SAFETY: `nid` is a fully initialized `NOTIFYICONDATAW` targeting our own HWND.
                return unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 };
            }
            let payload = Box::into_raw(Box::new((title.to_owned(), body.to_owned())));
            // SAFETY: Transfers ownership of `payload` to `wnd_proc(WM_SHOW_EPHEMERAL_NOTIFY)`.
            let posted =
                unsafe { PostMessageW(self.hwnd, WM_SHOW_EPHEMERAL_NOTIFY, 0, payload as isize) };
            if posted == 0 {
                // SAFETY: Reclaim box if `PostMessageW` failed.
                unsafe {
                    drop(Box::from_raw(payload));
                }
                false
            } else {
                true
            }
        }
    }

    impl Drop for TrayHandle {
        fn drop(&mut self) {
            unsafe {
                PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            }
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use win32::TrayHandle;

#[cfg(unix)]
pub struct TrayHandle {
    _ipc_server: Option<crate::unix_platform::UnixInstanceServer>,
}

#[cfg(unix)]
impl TrayHandle {
    pub fn start(
        config: TrayConfig,
        on_action: impl Fn(TrayAction) + Send + 'static,
    ) -> Result<Self, String> {
        let server = if !config.instance_window_tag.trim().is_empty() {
            match crate::unix_platform::UnixInstanceServer::start(
                &config.instance_window_tag,
                move |raw| {
                    if let Some(payload) =
                        decode_copydata_payload(QV_COPYDATA_MAGIC, raw.as_bytes())
                    {
                        on_action(TrayAction::SecondInstance(payload));
                    }
                },
            ) {
                Ok(srv) => Some(srv),
                Err(err) => {
                    eprintln!("[unix-ipc] {err}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            _ipc_server: server,
        })
    }

    pub fn has_tray_icon(&self) -> bool {
        false
    }

    pub fn show_notification(&self, title: &str, body: &str) -> bool {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            crate::unix_platform::show_notification(title, body)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (title, body);
            false
        }
    }
}

#[cfg(not(any(target_os = "windows", unix)))]
pub struct TrayHandle;

#[cfg(not(any(target_os = "windows", unix)))]
impl TrayHandle {
    pub fn start(
        _config: TrayConfig,
        _on_action: impl Fn(TrayAction) + Send + 'static,
    ) -> Result<Self, String> {
        Err("システムトレイ / OS イベント常駐はこの OS では未対応です".to_owned())
    }

    pub fn has_tray_icon(&self) -> bool {
        false
    }

    pub fn show_notification(&self, _title: &str, _body: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_menu_ids_map_roundtrip() {
        assert_eq!(
            action_from_menu_id(CMD_TOGGLE),
            Some(TrayAction::ToggleVisible)
        );
        assert_eq!(action_from_menu_id(CMD_CHIPS), Some(TrayAction::ShowChips));
        assert_eq!(
            action_from_menu_id(CMD_FLYOUT),
            Some(TrayAction::ShowFlyout)
        );
        assert_eq!(action_from_menu_id(CMD_QUIT), Some(TrayAction::Quit));
        assert_eq!(action_from_menu_id(0), None);
        assert_eq!(action_from_menu_id(9999), None);
    }

    #[test]
    fn power_broadcast_maps_known_events() {
        assert_eq!(power_event_from_wparam(PBT_APMSUSPEND), Some("suspend"));
        assert_eq!(
            power_event_from_wparam(PBT_APMRESUMEAUTOMATIC),
            Some("resume")
        );
        assert_eq!(
            power_event_from_wparam(PBT_APMPOWERSTATUSCHANGE),
            Some("power-status")
        );
        assert_eq!(power_event_from_wparam(0x9999), None);
    }

    #[test]
    fn os_event_window_name_and_copydata_validation() {
        assert_eq!(
            os_event_window_name("Local\\QuantumView.Workspace"),
            "QuantumViewOsEventWindow.LocalQuantumViewWorkspace"
        );
        assert_eq!(os_event_window_name(""), "QuantumViewOsEventWindow.Default");

        let payload = br#"{"argv":["--workspace"],"deepLink":"qvfolio://open"}"#;
        assert_eq!(
            decode_copydata_payload(QV_COPYDATA_MAGIC, payload).as_deref(),
            Some(r#"{"argv":["--workspace"],"deepLink":"qvfolio://open"}"#)
        );
        assert!(decode_copydata_payload(0x1234, payload).is_none());
        assert!(decode_copydata_payload(QV_COPYDATA_MAGIC, b"not-json").is_none());
        assert!(decode_copydata_payload(QV_COPYDATA_MAGIC, b"").is_none());
    }

    #[test]
    fn tray_tip_and_notification_buffers_truncate_safely() {
        let short = encode_tray_tip("QuantumView");
        assert_eq!(short[11], 0);
        let decoded = String::from_utf16(&short[..11]).unwrap();
        assert_eq!(decoded, "QuantumView");

        let mut boundary = "a".repeat(126);
        boundary.push('😀');
        let encoded = encode_tray_tip(&boundary);
        let nul_pos = encoded.iter().position(|&c| c == 0).unwrap();
        assert_eq!(nul_pos, 126);
        assert_eq!(
            String::from_utf16(&encoded[..nul_pos]).unwrap(),
            "a".repeat(126)
        );

        let title = encode_wide_fixed::<64>(&format!("{}😀", "x".repeat(62)));
        let title_nul = title.iter().position(|&c| c == 0).unwrap();
        assert_eq!(title_nul, 62);
    }
}
