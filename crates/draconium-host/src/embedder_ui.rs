//! Native OS embedder controls, external URL launcher, and screenshot
//! verification helpers (Milestone C).
//!
//! All data transformations (UTF-16 encoding, Win32 file-filter construction,
//! multi-select buffer parsing, `COLORREF` conversion, menu entry layout, and
//! RGBA screenshot sanity checks) are dependency-free pure Rust so they can
//! be unit-tested on any OS with `rustc --edition 2024 --test`.
//!
//! On Windows, the thin `#[cfg(target_os = "windows")]` FFI layer binds
//! directly to `user32`, `comdlg32`, and `shell32` using the exact ABI
//! definitions from `windows-sys 0.61`, requiring zero additions to
//! `Cargo.toml` or `Cargo.lock`.

use std::path::PathBuf;

/// A single entry in a native popup menu (shared by `ContextMenu` and
/// `SelectElement`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupMenuEntry {
    pub id: u32,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
    pub separator: bool,
}

impl PopupMenuEntry {
    pub fn item(id: u32, label: impl Into<String>, enabled: bool, checked: bool) -> Self {
        Self {
            id,
            label: label.into(),
            enabled,
            checked,
            separator: false,
        }
    }

    pub fn separator() -> Self {
        Self {
            id: 0,
            label: String::new(),
            enabled: false,
            checked: false,
            separator: true,
        }
    }
}

/// Summary of a captured RGBA screenshot for automated visual verification
/// (`WebView::take_screenshot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenshotReport {
    pub width: u32,
    pub height: u32,
    pub sampled_pixels: usize,
    pub distinct_colors: usize,
    pub non_blank: bool,
}

/// Inspect raw RGBA8888 pixels from `servo::RgbaImage::as_raw()` to verify
/// that a rendered frame is non-empty and not a single flat solid color.
pub fn analyze_rgba_image(width: u32, height: u32, rgba: &[u8]) -> ScreenshotReport {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if width == 0 || height == 0 || rgba.len() < expected {
        return ScreenshotReport {
            width,
            height,
            sampled_pixels: 0,
            distinct_colors: 0,
            non_blank: false,
        };
    }
    let total_pixels = (width as usize) * (height as usize);
    let stride = (total_pixels / 4096).max(1);
    let mut distinct = Vec::<u32>::new();
    let mut sampled = 0usize;
    let mut idx = 0usize;
    while idx < total_pixels {
        let base = idx * 4;
        let pixel =
            u32::from_le_bytes([rgba[base], rgba[base + 1], rgba[base + 2], rgba[base + 3]]);
        sampled += 1;
        if distinct.len() < 64 && !distinct.contains(&pixel) {
            distinct.push(pixel);
        }
        idx += stride;
    }
    ScreenshotReport {
        width,
        height,
        sampled_pixels: sampled,
        distinct_colors: distinct.len(),
        non_blank: distinct.len() >= 2,
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 vector (stripping interior
/// NULs so Win32 APIs never truncate unexpectedly).
pub fn encode_wide_nul(text: &str) -> Vec<u16> {
    let mut out: Vec<u16> = text
        .chars()
        .filter(|&ch| ch != '\0')
        .flat_map(|ch| {
            let mut buf = [0u16; 2];
            ch.encode_utf16(&mut buf).to_vec()
        })
        .collect();
    out.push(0);
    out
}

/// Sanitize extension strings (e.g. `"png"` or `".png"`) and build a Win32
/// `OPENFILENAMEW.lpstrFilter` double-NUL-terminated UTF-16 buffer.
pub fn build_file_filter_wide(extensions: &[String]) -> Vec<u16> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in extensions {
        let ext = raw.trim().trim_start_matches('.');
        if !ext.is_empty()
            && ext
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            let pattern = format!("*.{}", ext.to_ascii_lowercase());
            if !cleaned.contains(&pattern) {
                cleaned.push(pattern);
            }
        }
    }
    let mut wide = Vec::<u16>::new();
    if !cleaned.is_empty() {
        let joined = cleaned.join(";");
        let label = format!("対応ファイル ({joined})");
        wide.extend(label.encode_utf16());
        wide.push(0);
        wide.extend(joined.encode_utf16());
        wide.push(0);
    }
    wide.extend("すべてのファイル (*.*)".encode_utf16());
    wide.push(0);
    wide.extend("*.*".encode_utf16());
    wide.push(0);
    wide.push(0);
    wide
}

/// Parse the UTF-16 buffer returned by `GetOpenFileNameW` (with
/// `OFN_EXPLORER`).
///
/// - Single selection: `"C:\\dir\\file.txt\0\0"` → `[PathBuf("C:\\dir\\file.txt")]`
/// - Multi selection: `"C:\\dir\0a.txt\0b.txt\0\0"` → `[PathBuf("C:\\dir\\a.txt"), PathBuf("C:\\dir\\b.txt")]`
pub fn parse_open_file_buffer(buf: &[u16]) -> Vec<PathBuf> {
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0usize;
    while start < buf.len() {
        let mut end = start;
        while end < buf.len() && buf[end] != 0 {
            end += 1;
        }
        if end == start {
            break;
        }
        parts.push(String::from_utf16_lossy(&buf[start..end]));
        start = end + 1;
    }
    match parts.as_slice() {
        [] => Vec::new(),
        [single] => vec![PathBuf::from(single)],
        [dir, files @ ..] => {
            let base = PathBuf::from(dir);
            files.iter().map(|name| base.join(name)).collect()
        }
    }
}

/// Convert RGB components to a Win32 `COLORREF` (`0x00BBGGRR`).
pub const fn rgb_to_colorref(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// Convert a Win32 `COLORREF` (`0x00BBGGRR`) back to `(r, g, b)`.
pub const fn colorref_to_rgb(colorref: u32) -> (u8, u8, u8) {
    (
        (colorref & 0xff) as u8,
        ((colorref >> 8) & 0xff) as u8,
        ((colorref >> 16) & 0xff) as u8,
    )
}

/// Pre-fill a `GetSaveFileNameW` UTF-16 buffer with a suggested filename,
/// guaranteed to be NUL-terminated and never splitting a UTF-16 surrogate pair.
pub fn prefill_file_buffer(default_name: &str, capacity: usize) -> Vec<u16> {
    let mut buf = vec![0u16; capacity.max(2)];
    let mut len = 0usize;
    for ch in default_name.chars() {
        if ch == '\0' {
            continue;
        }
        let mut units = [0u16; 2];
        let encoded = ch.encode_utf16(&mut units);
        if len + encoded.len() >= buf.len() {
            break;
        }
        buf[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    buf[len] = 0;
    buf
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr;

    use super::{
        PopupMenuEntry, build_file_filter_wide, colorref_to_rgb, encode_wide_nul,
        parse_open_file_buffer, prefill_file_buffer, rgb_to_colorref,
    };

    pub type Hwnd = isize;
    type Hmenu = *mut c_void;
    type Hinstance = isize;
    type ColorRef = u32;

    const MB_OK: u32 = 0x0000_0000;
    const MB_OKCANCEL: u32 = 0x0000_0001;
    const MB_ICONQUESTION: u32 = 0x0000_0020;
    const MB_ICONINFORMATION: u32 = 0x0000_0040;
    const IDOK: i32 = 1;

    const MF_STRING: u32 = 0x0000_0000;
    const MF_GRAYED: u32 = 0x0000_0001;
    const MF_DISABLED: u32 = 0x0000_0002;
    const MF_CHECKED: u32 = 0x0000_0008;
    const MF_SEPARATOR: u32 = 0x0000_0800;

    const TPM_LEFTALIGN: u32 = 0x0000_0000;
    const TPM_TOPALIGN: u32 = 0x0000_0000;
    const TPM_RIGHTBUTTON: u32 = 0x0000_0002;
    const TPM_NONOTIFY: u32 = 0x0000_0080;
    const TPM_RETURNCMD: u32 = 0x0000_0100;
    const WM_NULL: u32 = 0x0000;

    const OFN_OVERWRITEPROMPT: u32 = 0x0000_0002;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    const OFN_ALLOWMULTISELECT: u32 = 0x0000_0200;
    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_EXPLORER: u32 = 0x0008_0000;

    const CC_RGBINIT: u32 = 0x0000_0001;
    const CC_FULLOPEN: u32 = 0x0000_0002;
    const SW_SHOWNORMAL: i32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[cfg_attr(target_arch = "x86", repr(packed(1)))]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: Hwnd,
        h_instance: Hinstance,
        lpstr_filter: *const u16,
        lpstr_custom_filter: *mut u16,
        n_max_cust_filter: u32,
        n_filter_index: u32,
        lpstr_file: *mut u16,
        n_max_file: u32,
        lpstr_file_title: *mut u16,
        n_max_file_title: u32,
        lpstr_initial_dir: *const u16,
        lpstr_title: *const u16,
        flags: u32,
        n_file_offset: u16,
        n_file_extension: u16,
        lpstr_def_ext: *const u16,
        l_cust_data: isize,
        lpfn_hook: *const c_void,
        lp_template_name: *const u16,
        pv_reserved: *mut c_void,
        dw_reserved: u32,
        flags_ex: u32,
    }

    #[repr(C)]
    #[cfg_attr(target_arch = "x86", repr(packed(1)))]
    struct ChooseColorW {
        l_struct_size: u32,
        hwnd_owner: Hwnd,
        h_instance: Hwnd,
        rgb_result: ColorRef,
        lp_cust_colors: *mut ColorRef,
        flags: u32,
        l_cust_data: isize,
        lpfn_hook: *const c_void,
        lp_template_name: *const u16,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, utype: u32) -> i32;
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
        fn ClientToScreen(hwnd: Hwnd, point: *mut Point) -> i32;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    #[link(name = "comdlg32")]
    unsafe extern "system" {
        fn GetOpenFileNameW(lpofn: *mut OpenFileNameW) -> i32;
        fn GetSaveFileNameW(lpofn: *mut OpenFileNameW) -> i32;
        fn ChooseColorW(lpcc: *mut ChooseColorW) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: Hwnd,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(pv_reserved: *mut c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            rclsid: *const Guid,
            p_unk_outer: *mut c_void,
            dw_cls_context: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
        fn CoTaskMemFree(pv: *mut c_void);
    }

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const CLSID_FILE_OPEN_DIALOG: Guid = Guid {
        data1: 0xDC1C_5A9C,
        data2: 0xE88A,
        data3: 0x4DDE,
        data4: [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
    };

    const IID_IFILE_OPEN_DIALOG: Guid = Guid {
        data1: 0xD57C_7288,
        data2: 0xD4AD,
        data3: 0x4768,
        data4: [0xBE, 0x02, 0x9D, 0x96, 0x95, 0x32, 0xD9, 0x60],
    };

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
    const FOS_PICKFOLDERS: u32 = 0x0000_0020;
    const FOS_FORCEFILESYSTEM: u32 = 0x0000_0040;
    const FOS_PATHMUSTEXIST: u32 = 0x0000_0800;
    const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

    pub fn show_select_folder_dialog(hwnd: Hwnd, title: &str) -> Option<PathBuf> {
        unsafe {
            let hr_init = CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED);
            let should_uninit = hr_init >= 0;
            if hr_init < 0 && hr_init != RPC_E_CHANGED_MODE {
                return None;
            }

            let mut dialog: *mut c_void = ptr::null_mut();
            let hr_create = CoCreateInstance(
                &CLSID_FILE_OPEN_DIALOG,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IFILE_OPEN_DIALOG,
                &mut dialog,
            );
            if hr_create < 0 || dialog.is_null() {
                if should_uninit {
                    CoUninitialize();
                }
                return None;
            }

            let vtbl = *(dialog as *const *const *const c_void);
            let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vtbl.add(2));
            let show_fn: unsafe extern "system" fn(*mut c_void, Hwnd) -> i32 =
                std::mem::transmute(*vtbl.add(3));
            let set_options_fn: unsafe extern "system" fn(*mut c_void, u32) -> i32 =
                std::mem::transmute(*vtbl.add(9));
            let get_options_fn: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                std::mem::transmute(*vtbl.add(10));
            let set_title_fn: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                std::mem::transmute(*vtbl.add(17));
            let get_result_fn: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                std::mem::transmute(*vtbl.add(20));

            let mut opts: u32 = 0;
            let _ = get_options_fn(dialog, &mut opts);
            let _ = set_options_fn(
                dialog,
                opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            );
            if !title.is_empty() {
                let title_w = encode_wide_nul(title);
                let _ = set_title_fn(dialog, title_w.as_ptr());
            }

            let mut selected: Option<PathBuf> = None;
            if show_fn(dialog, hwnd) >= 0 {
                let mut shell_item: *mut c_void = ptr::null_mut();
                if get_result_fn(dialog, &mut shell_item) >= 0 && !shell_item.is_null() {
                    let item_vtbl = *(shell_item as *const *const *const c_void);
                    let item_release: unsafe extern "system" fn(*mut c_void) -> u32 =
                        std::mem::transmute(*item_vtbl.add(2));
                    let get_display_name: unsafe extern "system" fn(
                        *mut c_void,
                        u32,
                        *mut *mut u16,
                    ) -> i32 = std::mem::transmute(*item_vtbl.add(5));

                    let mut path_ptr: *mut u16 = ptr::null_mut();
                    if get_display_name(shell_item, SIGDN_FILESYSPATH, &mut path_ptr) >= 0
                        && !path_ptr.is_null()
                    {
                        let mut len = 0usize;
                        while *path_ptr.add(len) != 0 {
                            len += 1;
                        }
                        let slice = std::slice::from_raw_parts(path_ptr, len);
                        use std::os::windows::ffi::OsStringExt;
                        selected = Some(PathBuf::from(std::ffi::OsString::from_wide(slice)));
                        CoTaskMemFree(path_ptr as *mut c_void);
                    }
                    let _ = item_release(shell_item);
                }
            }

            let _ = release_fn(dialog);
            if should_uninit {
                CoUninitialize();
            }
            selected
        }
    }

    pub fn show_alert(hwnd: Hwnd, title: &str, message: &str) {
        let title_w = encode_wide_nul(title);
        let message_w = encode_wide_nul(message);
        // SAFETY: Both pointers reference valid NUL-terminated UTF-16 buffers.
        unsafe {
            MessageBoxW(
                hwnd,
                message_w.as_ptr(),
                title_w.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    pub fn show_confirm(hwnd: Hwnd, title: &str, message: &str) -> bool {
        let title_w = encode_wide_nul(title);
        let message_w = encode_wide_nul(message);
        // SAFETY: Both pointers reference valid NUL-terminated UTF-16 buffers.
        let res = unsafe {
            MessageBoxW(
                hwnd,
                message_w.as_ptr(),
                title_w.as_ptr(),
                MB_OKCANCEL | MB_ICONQUESTION,
            )
        };
        res == IDOK
    }

    pub fn show_popup_menu(
        hwnd: Hwnd,
        client_x: i32,
        client_y: i32,
        entries: &[PopupMenuEntry],
    ) -> Option<u32> {
        if entries.is_empty() {
            return None;
        }
        // SAFETY: Standard Win32 popup menu lifecycle on the UI thread.
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return None;
            }
            for entry in entries {
                if entry.separator {
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                } else {
                    let mut flags = MF_STRING;
                    if !entry.enabled {
                        flags |= MF_DISABLED | MF_GRAYED;
                    }
                    if entry.checked {
                        flags |= MF_CHECKED;
                    }
                    let label_w = encode_wide_nul(&entry.label);
                    AppendMenuW(menu, flags, entry.id as usize, label_w.as_ptr());
                }
            }
            let mut pt = Point {
                x: client_x,
                y: client_y,
            };
            ClientToScreen(hwnd, &mut pt);
            SetForegroundWindow(hwnd);
            let selected = TrackPopupMenuEx(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                hwnd,
                ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            if selected > 0 {
                Some(selected as u32)
            } else {
                None
            }
        }
    }

    pub fn show_open_file_dialog(
        hwnd: Hwnd,
        extensions: &[String],
        allow_multiple: bool,
    ) -> Option<Vec<PathBuf>> {
        let filter = build_file_filter_wide(extensions);
        let mut file_buf = vec![0u16; 32_768];
        let mut flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
        if allow_multiple {
            flags |= OFN_ALLOWMULTISELECT;
        }
        let mut ofn = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            hwnd_owner: hwnd,
            h_instance: 0,
            lpstr_filter: filter.as_ptr(),
            lpstr_custom_filter: ptr::null_mut(),
            n_max_cust_filter: 0,
            n_filter_index: 1,
            lpstr_file: file_buf.as_mut_ptr(),
            n_max_file: file_buf.len() as u32,
            lpstr_file_title: ptr::null_mut(),
            n_max_file_title: 0,
            lpstr_initial_dir: ptr::null(),
            lpstr_title: ptr::null(),
            flags,
            n_file_offset: 0,
            n_file_extension: 0,
            lpstr_def_ext: ptr::null(),
            l_cust_data: 0,
            lpfn_hook: ptr::null(),
            lp_template_name: ptr::null(),
            pv_reserved: ptr::null_mut(),
            dw_reserved: 0,
            flags_ex: 0,
        };
        // SAFETY: `ofn` points to valid buffers that live through the call.
        let ok = unsafe { GetOpenFileNameW(&mut ofn) };
        if ok == 0 {
            return None;
        }
        let paths = parse_open_file_buffer(&file_buf);
        if paths.is_empty() { None } else { Some(paths) }
    }

    pub fn show_save_file_dialog(
        hwnd: Hwnd,
        default_name: &str,
        extensions: &[String],
    ) -> Option<PathBuf> {
        let filter = build_file_filter_wide(extensions);
        let mut file_buf = prefill_file_buffer(default_name, 4096);
        let def_ext = extensions
            .first()
            .map(|e| e.trim().trim_start_matches('.'))
            .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(encode_wide_nul);
        let mut ofn = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            hwnd_owner: hwnd,
            h_instance: 0,
            lpstr_filter: filter.as_ptr(),
            lpstr_custom_filter: ptr::null_mut(),
            n_max_cust_filter: 0,
            n_filter_index: 1,
            lpstr_file: file_buf.as_mut_ptr(),
            n_max_file: file_buf.len() as u32,
            lpstr_file_title: ptr::null_mut(),
            n_max_file_title: 0,
            lpstr_initial_dir: ptr::null(),
            lpstr_title: ptr::null(),
            flags: OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR,
            n_file_offset: 0,
            n_file_extension: 0,
            lpstr_def_ext: def_ext.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
            l_cust_data: 0,
            lpfn_hook: ptr::null(),
            lp_template_name: ptr::null(),
            pv_reserved: ptr::null_mut(),
            dw_reserved: 0,
            flags_ex: 0,
        };
        // SAFETY: `ofn` and all referenced buffers remain valid for the duration of the dialog.
        let ok = unsafe { GetSaveFileNameW(&mut ofn) };
        if ok == 0 {
            return None;
        }
        parse_open_file_buffer(&file_buf).into_iter().next()
    }

    pub fn show_color_picker(hwnd: Hwnd, initial: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
        let mut custom = [0x00ff_ffffu32; 16];
        let mut cc = ChooseColorW {
            l_struct_size: std::mem::size_of::<ChooseColorW>() as u32,
            hwnd_owner: hwnd,
            h_instance: 0,
            rgb_result: rgb_to_colorref(initial.0, initial.1, initial.2),
            lp_cust_colors: custom.as_mut_ptr(),
            flags: CC_RGBINIT | CC_FULLOPEN,
            l_cust_data: 0,
            lpfn_hook: ptr::null(),
            lp_template_name: ptr::null(),
        };
        // SAFETY: `cc` and `custom` remain valid for the duration of the modal dialog.
        let ok = unsafe { ChooseColorW(&mut cc) };
        if ok == 0 {
            None
        } else {
            Some(colorref_to_rgb(cc.rgb_result))
        }
    }

    pub fn open_external_url(url: &str) -> Result<(), String> {
        let op = encode_wide_nul("open");
        let target = encode_wide_nul(url);
        // SAFETY: Both pointers are valid NUL-terminated UTF-16 strings.
        // Callers must validate `url` via `bridge::allows_external_url` first.
        let res = unsafe {
            ShellExecuteW(
                0,
                op.as_ptr(),
                target.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if res > 32 {
            Ok(())
        } else {
            Err(format!("外部 URL の起動に失敗しました (code={res})"))
        }
    }
}

/// Show a native alert dialog (or log in headless/unsupported mode).
pub fn show_alert(hwnd: isize, title: &str, message: &str, non_interactive: bool) {
    if non_interactive {
        eprintln!("[alert] {title}: {message}");
        return;
    }
    #[cfg(target_os = "windows")]
    win32::show_alert(hwnd, title, message);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_alert(title, message);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        eprintln!("[alert] {title}: {message}");
    }
}

/// Show a native OK/Cancel confirmation dialog (returns `false` in
/// headless/unsupported mode).
pub fn show_confirm(hwnd: isize, title: &str, message: &str, non_interactive: bool) -> bool {
    if non_interactive {
        eprintln!("[confirm -> cancel] {title}: {message}");
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_confirm(hwnd, title, message)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_confirm(title, message)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = hwnd;
        eprintln!("[confirm -> cancel] {title}: {message}");
        false
    }
}

/// Show a native popup menu at client coordinates `(client_x, client_y)` and
/// return the selected entry's `id`, or `None` when dismissed.
pub fn show_popup_menu(
    hwnd: isize,
    client_x: i32,
    client_y: i32,
    entries: &[PopupMenuEntry],
    non_interactive: bool,
) -> Option<u32> {
    if non_interactive || entries.is_empty() {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_popup_menu(hwnd, client_x, client_y, entries)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd, client_x, client_y);
        None
    }
}

/// Show a native file-open dialog for `<input type="file">`.
pub fn show_open_file_dialog(
    hwnd: isize,
    extensions: &[String],
    allow_multiple: bool,
    non_interactive: bool,
) -> Option<Vec<PathBuf>> {
    if non_interactive {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_open_file_dialog(hwnd, extensions, allow_multiple)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_open_file_dialog(extensions, allow_multiple)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, extensions, allow_multiple);
        None
    }
}

/// Show a native file-save dialog for `<a download>`,
/// `Draconium.dialog.saveTextFile/saveBase64File`, and PNG capture fallback.
pub fn show_save_file_dialog(
    hwnd: isize,
    default_name: &str,
    extensions: &[String],
    non_interactive: bool,
) -> Option<PathBuf> {
    if non_interactive {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_save_file_dialog(hwnd, default_name, extensions)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_save_file_dialog(default_name, extensions)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, default_name, extensions);
        None
    }
}

/// Show a native folder-selection dialog (`IFileOpenDialog` on Windows,
/// `choose folder` on macOS, `zenity --directory` on Linux).
pub fn show_select_folder_dialog(
    hwnd: isize,
    title: &str,
    non_interactive: bool,
) -> Option<PathBuf> {
    if non_interactive {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_select_folder_dialog(hwnd, title)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_select_folder_dialog(title)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, title);
        None
    }
}

/// Show a native color picker for `<input type="color">`.
pub fn show_color_picker(
    hwnd: isize,
    initial: (u8, u8, u8),
    non_interactive: bool,
) -> Option<(u8, u8, u8)> {
    if non_interactive {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        win32::show_color_picker(hwnd, initial)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let _ = hwnd;
        crate::unix_platform::show_color_picker(initial)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (hwnd, initial);
        None
    }
}

/// Open an already-validated external URL (`https:` / `mailto:`) in the OS
/// default browser/handler.
pub fn open_external_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::open_external_url(url)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::unix_platform::open_external_url(url)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
        Err("外部 URL 起動はこの OS では未対応です".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_encoding_strips_interior_nuls_and_terminates() {
        let wide = encode_wide_nul("A\0日本😀");
        assert_eq!(*wide.last().unwrap(), 0);
        assert!(!wide[..wide.len() - 1].contains(&0));
        let decoded = String::from_utf16(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(decoded, "A日本😀");
    }

    #[test]
    fn prefill_file_buffer_preserves_name_and_surrogates() {
        let buf = prefill_file_buffer("note.md", 32);
        let nul = buf.iter().position(|&c| c == 0).unwrap();
        assert_eq!(String::from_utf16(&buf[..nul]).unwrap(), "note.md");

        let tiny = prefill_file_buffer("ab😀", 4); // slots: 'a', 'b', NUL (emoji needs 2)
        let tiny_nul = tiny.iter().position(|&c| c == 0).unwrap();
        assert_eq!(String::from_utf16(&tiny[..tiny_nul]).unwrap(), "ab");
    }

    #[test]
    fn file_filter_sanitizes_and_double_terminates() {
        let filter = build_file_filter_wide(&[
            ".PNG".to_owned(),
            "jpg".to_owned(),
            "png".to_owned(),
            "../evil".to_owned(),
        ]);
        assert!(filter.ends_with(&[0, 0]));
        let text = String::from_utf16_lossy(&filter);
        assert!(text.contains("*.png;*.jpg"));
        assert!(!text.contains("evil"));
        assert!(text.contains("*.*"));
    }

    #[test]
    fn open_file_buffer_parses_single_and_multi_selection() {
        let mut single: Vec<u16> = "C:\\work\\note.md".encode_utf16().collect();
        single.extend([0, 0]);
        assert_eq!(
            parse_open_file_buffer(&single),
            vec![PathBuf::from("C:\\work\\note.md")]
        );

        let mut multi: Vec<u16> = "C:\\work".encode_utf16().collect();
        multi.push(0);
        multi.extend("a.txt".encode_utf16());
        multi.push(0);
        multi.extend("b.png".encode_utf16());
        multi.extend([0, 0]);
        assert_eq!(
            parse_open_file_buffer(&multi),
            vec![
                PathBuf::from("C:\\work").join("a.txt"),
                PathBuf::from("C:\\work").join("b.png"),
            ]
        );

        assert!(parse_open_file_buffer(&[0, 0]).is_empty());
    }

    #[test]
    fn colorref_roundtrip() {
        let raw = rgb_to_colorref(0x12, 0x34, 0x56);
        assert_eq!(raw, 0x0056_3412);
        assert_eq!(colorref_to_rgb(raw), (0x12, 0x34, 0x56));
    }

    #[test]
    fn screenshot_analysis_detects_blank_and_rendered_frames() {
        let blank = vec![0x11, 0x22, 0x33, 0xff].repeat(16);
        let report_blank = analyze_rgba_image(4, 4, &blank);
        assert!(!report_blank.non_blank);
        assert_eq!(report_blank.distinct_colors, 1);

        let mut rendered = blank;
        rendered[0] = 0xff;
        let report_ok = analyze_rgba_image(4, 4, &rendered);
        assert!(report_ok.non_blank);
        assert_eq!(report_ok.distinct_colors, 2);

        let empty = analyze_rgba_image(0, 10, &[]);
        assert!(!empty.non_blank);
    }
}
