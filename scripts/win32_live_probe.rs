#![allow(dead_code, unused_imports)]

#[path = "../crates/draconium-host/src/win_platform.rs"]
mod win_platform;

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let out_root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("draconium-win32-probe"));
    std::fs::create_dir_all(&out_root).expect("failed to create probe output directory");

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_millis();

    // 1. DPAPI CryptProtectData / CryptUnprotectData round-trip
    let secret = "秘密データ-日本語-🔑-DPAPI-test-0123456789".as_bytes();
    let enc = win_platform::dpapi_encrypt(secret).expect("DPAPI encrypt failed");
    assert_ne!(enc, secret, "ciphertext must differ from plaintext");
    let dec = win_platform::dpapi_decrypt(&enc).expect("DPAPI decrypt failed");
    assert_eq!(dec, secret, "DPAPI roundtrip mismatch");
    println!("WIN32_DPAPI_OK cipher_len={}", enc.len());

    // 2. Win32 Clipboard Text / CF_HTML / CF_DIB Image round-trip (preserving prior text)
    let saved_prev_text = win_platform::clipboard_read_text(0).ok();

    win_platform::clipboard_write_text(0, "Draconium実機テスト😀")
        .expect("clipboard_write_text failed");
    let got_text = win_platform::clipboard_read_text(0).expect("clipboard_read_text failed");
    assert_eq!(got_text, "Draconium実機テスト😀");
    println!("WIN32_CLIPBOARD_TEXT_OK");

    let html_frag = "<b>太字テスト</b><span>Draconium</span>";
    win_platform::clipboard_write_html(0, html_frag).expect("clipboard_write_html failed");
    let got_html = win_platform::clipboard_read_html(0).expect("clipboard_read_html failed");
    assert!(
        got_html.contains("太字テスト") && got_html.contains("Draconium"),
        "unexpected CF_HTML roundtrip: {got_html}"
    );
    println!("WIN32_CLIPBOARD_HTML_OK");

    let rgba: Vec<u8> = vec![
        255, 0, 0, 255, // red
        0, 255, 0, 255, // green
        0, 0, 255, 255, // blue
        255, 255, 255, 255, // white
    ];
    let dib = win_platform::rgba_to_dib(2, 2, &rgba).expect("rgba_to_dib failed");
    win_platform::clipboard_write_image_dib(0, &dib).expect("clipboard_write_image_dib failed");
    let img_json =
        win_platform::clipboard_read_image_json(0).expect("clipboard_read_image_json failed");
    assert!(
        img_json.contains("\"width\":2")
            && img_json.contains("\"height\":2")
            && img_json.contains("data:image/png;base64,"),
        "unexpected clipboard image payload: {img_json}"
    );
    println!("WIN32_CLIPBOARD_DIB_OK len={}", img_json.len());

    win_platform::clipboard_clear(0).expect("clipboard_clear failed");
    let cleared = win_platform::clipboard_read_text(0).expect("read after clear failed");
    assert_eq!(cleared, "");
    if let Some(prev) = saved_prev_text
        && !prev.is_empty()
    {
        let _ = win_platform::clipboard_write_text(0, &prev);
    }
    println!("WIN32_CLIPBOARD_CLEAR_OK");

    // 3. HKCU Registry Autostart round-trip (temporary key + cleanup)
    let app_name = format!("DraconiumVerifyTemp{unique}");
    assert!(
        !win_platform::autostart_is_enabled(&app_name).expect("is_enabled initial failed"),
        "fresh unique autostart key should not exist"
    );
    win_platform::autostart_set_enabled(
        &app_name,
        true,
        r#""C:\NonExistent\draconium.exe" --starter"#,
    )
    .expect("autostart enable failed");
    let enabled_ok = win_platform::autostart_is_enabled(&app_name);
    let disable_res = win_platform::autostart_set_enabled(&app_name, false, "");
    assert_eq!(enabled_ok.expect("is_enabled after set failed"), true);
    disable_res.expect("autostart disable cleanup failed");
    assert!(
        !win_platform::autostart_is_enabled(&app_name).expect("is_enabled after remove failed"),
        "autostart key remained after cleanup"
    );
    println!("WIN32_AUTOSTART_HKCU_OK");

    // 4. HKCU Custom URI Scheme registration + unregister cleanup
    let scheme = format!("dnverify{}", std::process::id());
    win_platform::register_uri_scheme(
        &scheme,
        r#""C:\NonExistent\draconium.exe" --deep-link "%1""#,
    )
    .expect("register_uri_scheme failed");
    let reg_out = Command::new("reg.exe")
        .args([
            "query",
            &format!(r"HKCU\Software\Classes\{scheme}\shell\open\command"),
        ])
        .output()
        .expect("reg query failed");
    let unregister_res = win_platform::unregister_uri_scheme(&scheme);
    assert!(
        reg_out.status.success(),
        "URI scheme registry key not found"
    );
    unregister_res.expect("unregister_uri_scheme cleanup failed");
    let reg_after = Command::new("reg.exe")
        .args(["query", &format!(r"HKCU\Software\Classes\{scheme}")])
        .output()
        .expect("reg query after unregister failed");
    assert!(
        !reg_after.status.success(),
        "URI scheme registry key still exists after unregister"
    );
    println!("WIN32_PROTOCOL_CLIENT_HKCU_OK");

    // 5. Raster PDF generator verification (%PDF-1.4 structure)
    let pdf = win_platform::encode_rgba_as_raster_pdf(2, 2, &rgba).expect("pdf encode failed");
    assert!(pdf.starts_with(b"%PDF-1.4\n") && pdf.ends_with(b"%%EOF\n"));
    let pdf_path = out_root.join("test-out.pdf");
    std::fs::write(&pdf_path, &pdf).expect("write pdf failed");
    println!("WIN32_PDF_GEN_OK bytes={}", pdf.len());

    // 6. Structured CrashReport writer + URL redaction + 10-file rotation
    let crash_dir = out_root.join("crashes-unit");
    let _ = std::fs::remove_dir_all(&crash_dir);
    for idx in 0..12 {
        let mut rpt = win_platform::CrashReport::new(
            "Draconium Verify",
            "dn://app/workspace.html?secret=token123#frag",
            "rust-panic",
            &format!("simulated panic #{idx}"),
            Some("host.rs:100:1".to_owned()),
        );
        rpt.timestamp_unix_ms += idx as u128;
        win_platform::write_crash_report(&crash_dir, &rpt).expect("write_crash_report failed");
    }
    let crash_files: Vec<_> = std::fs::read_dir(&crash_dir)
        .expect("read crash_dir failed")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        crash_files.len(),
        10,
        "crash rotation must retain exactly 10 files"
    );
    let sample_crash =
        std::fs::read_to_string(crash_files[0].path()).expect("read sample crash failed");
    assert!(!sample_crash.contains("secret=token123") && !sample_crash.contains("#frag"));
    assert!(sample_crash.contains("\"url\": \"dn://app/workspace.html\""));
    assert!(sample_crash.contains("\"source\": \"rust-panic\""));
    println!("WIN32_CRASH_REPORT_ROTATION_OK count={}", crash_files.len());

    // 7. System theme detection
    let dark = win_platform::detect_system_dark_mode();
    assert!(matches!(dark, Some(true | false) | None));
    println!("WIN32_THEME_DETECT_OK dark={dark:?}");
}
