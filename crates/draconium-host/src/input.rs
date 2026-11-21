/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Input translation between winit 0.30 and Servo 0.8.0 (text editing,
// navigation, function keys, physical scan codes, and Japanese IME).
// Derived from Servo 0.8.0's desktop/keyutils.rs.
use servo::{Code, Cursor, Key, KeyState, KeyboardEvent, Location, Modifiers, NamedKey};
use winit::event::{ElementState, KeyEvent, MouseButton as WinitMouseButton};
use winit::keyboard::{
    Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey as WinitNamedKey, PhysicalKey,
};
use winit::window::{CursorIcon, Window};

fn named_key(named: &WinitNamedKey) -> NamedKey {
    match named {
        WinitNamedKey::Enter => NamedKey::Enter,
        WinitNamedKey::Tab => NamedKey::Tab,
        WinitNamedKey::Backspace => NamedKey::Backspace,
        WinitNamedKey::Delete => NamedKey::Delete,
        WinitNamedKey::Insert => NamedKey::Insert,
        WinitNamedKey::Escape => NamedKey::Escape,
        WinitNamedKey::ArrowUp => NamedKey::ArrowUp,
        WinitNamedKey::ArrowDown => NamedKey::ArrowDown,
        WinitNamedKey::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamedKey::ArrowRight => NamedKey::ArrowRight,
        WinitNamedKey::Home => NamedKey::Home,
        WinitNamedKey::End => NamedKey::End,
        WinitNamedKey::PageUp => NamedKey::PageUp,
        WinitNamedKey::PageDown => NamedKey::PageDown,
        WinitNamedKey::F1 => NamedKey::F1,
        WinitNamedKey::F2 => NamedKey::F2,
        WinitNamedKey::F3 => NamedKey::F3,
        WinitNamedKey::F4 => NamedKey::F4,
        WinitNamedKey::F5 => NamedKey::F5,
        WinitNamedKey::F6 => NamedKey::F6,
        WinitNamedKey::F7 => NamedKey::F7,
        WinitNamedKey::F8 => NamedKey::F8,
        WinitNamedKey::F9 => NamedKey::F9,
        WinitNamedKey::F10 => NamedKey::F10,
        WinitNamedKey::F11 => NamedKey::F11,
        WinitNamedKey::F12 => NamedKey::F12,
        WinitNamedKey::F13 => NamedKey::F13,
        WinitNamedKey::F14 => NamedKey::F14,
        WinitNamedKey::F15 => NamedKey::F15,
        WinitNamedKey::F16 => NamedKey::F16,
        WinitNamedKey::F17 => NamedKey::F17,
        WinitNamedKey::F18 => NamedKey::F18,
        WinitNamedKey::F19 => NamedKey::F19,
        WinitNamedKey::F20 => NamedKey::F20,
        WinitNamedKey::F21 => NamedKey::F21,
        WinitNamedKey::F22 => NamedKey::F22,
        WinitNamedKey::F23 => NamedKey::F23,
        WinitNamedKey::F24 => NamedKey::F24,
        WinitNamedKey::Shift => NamedKey::Shift,
        WinitNamedKey::Control => NamedKey::Control,
        WinitNamedKey::Alt => NamedKey::Alt,
        WinitNamedKey::AltGraph => NamedKey::AltGraph,
        WinitNamedKey::Super | WinitNamedKey::Meta => NamedKey::Meta,
        WinitNamedKey::CapsLock => NamedKey::CapsLock,
        WinitNamedKey::Convert => NamedKey::Convert,
        WinitNamedKey::NonConvert => NamedKey::NonConvert,
        WinitNamedKey::Hiragana => NamedKey::Hiragana,
        WinitNamedKey::Katakana => NamedKey::Katakana,
        WinitNamedKey::HiraganaKatakana => NamedKey::HiraganaKatakana,
        WinitNamedKey::ZenkakuHankaku => NamedKey::ZenkakuHankaku,
        WinitNamedKey::KanaMode => NamedKey::KanaMode,
        WinitNamedKey::KanjiMode => NamedKey::KanjiMode,
        WinitNamedKey::Romaji => NamedKey::Romaji,
        WinitNamedKey::Process => NamedKey::Process,
        _ => NamedKey::Unidentified,
    }
}

fn physical_code(physical_key: PhysicalKey) -> Code {
    let PhysicalKey::Code(code) = physical_key else {
        return Code::Unidentified;
    };
    match code {
        KeyCode::Backquote => Code::Backquote,
        KeyCode::Digit0 => Code::Digit0,
        KeyCode::Digit1 => Code::Digit1,
        KeyCode::Digit2 => Code::Digit2,
        KeyCode::Digit3 => Code::Digit3,
        KeyCode::Digit4 => Code::Digit4,
        KeyCode::Digit5 => Code::Digit5,
        KeyCode::Digit6 => Code::Digit6,
        KeyCode::Digit7 => Code::Digit7,
        KeyCode::Digit8 => Code::Digit8,
        KeyCode::Digit9 => Code::Digit9,
        KeyCode::KeyA => Code::KeyA,
        KeyCode::KeyB => Code::KeyB,
        KeyCode::KeyC => Code::KeyC,
        KeyCode::KeyD => Code::KeyD,
        KeyCode::KeyE => Code::KeyE,
        KeyCode::KeyF => Code::KeyF,
        KeyCode::KeyG => Code::KeyG,
        KeyCode::KeyH => Code::KeyH,
        KeyCode::KeyI => Code::KeyI,
        KeyCode::KeyJ => Code::KeyJ,
        KeyCode::KeyK => Code::KeyK,
        KeyCode::KeyL => Code::KeyL,
        KeyCode::KeyM => Code::KeyM,
        KeyCode::KeyN => Code::KeyN,
        KeyCode::KeyO => Code::KeyO,
        KeyCode::KeyP => Code::KeyP,
        KeyCode::KeyQ => Code::KeyQ,
        KeyCode::KeyR => Code::KeyR,
        KeyCode::KeyS => Code::KeyS,
        KeyCode::KeyT => Code::KeyT,
        KeyCode::KeyU => Code::KeyU,
        KeyCode::KeyV => Code::KeyV,
        KeyCode::KeyW => Code::KeyW,
        KeyCode::KeyX => Code::KeyX,
        KeyCode::KeyY => Code::KeyY,
        KeyCode::KeyZ => Code::KeyZ,
        KeyCode::Minus => Code::Minus,
        KeyCode::Equal => Code::Equal,
        KeyCode::BracketLeft => Code::BracketLeft,
        KeyCode::BracketRight => Code::BracketRight,
        KeyCode::Backslash => Code::Backslash,
        KeyCode::Semicolon => Code::Semicolon,
        KeyCode::Quote => Code::Quote,
        KeyCode::Comma => Code::Comma,
        KeyCode::Period => Code::Period,
        KeyCode::Slash => Code::Slash,
        KeyCode::Space => Code::Space,
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab => Code::Tab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Insert => Code::Insert,
        KeyCode::Escape => Code::Escape,
        KeyCode::ArrowUp => Code::ArrowUp,
        KeyCode::ArrowDown => Code::ArrowDown,
        KeyCode::ArrowLeft => Code::ArrowLeft,
        KeyCode::ArrowRight => Code::ArrowRight,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::F1 => Code::F1,
        KeyCode::F2 => Code::F2,
        KeyCode::F3 => Code::F3,
        KeyCode::F4 => Code::F4,
        KeyCode::F5 => Code::F5,
        KeyCode::F6 => Code::F6,
        KeyCode::F7 => Code::F7,
        KeyCode::F8 => Code::F8,
        KeyCode::F9 => Code::F9,
        KeyCode::F10 => Code::F10,
        KeyCode::F11 => Code::F11,
        KeyCode::F12 => Code::F12,
        KeyCode::F13 => Code::F13,
        KeyCode::F14 => Code::F14,
        KeyCode::F15 => Code::F15,
        KeyCode::F16 => Code::F16,
        KeyCode::F17 => Code::F17,
        KeyCode::F18 => Code::F18,
        KeyCode::F19 => Code::F19,
        KeyCode::F20 => Code::F20,
        KeyCode::F21 => Code::F21,
        KeyCode::F22 => Code::F22,
        KeyCode::F23 => Code::F23,
        KeyCode::F24 => Code::F24,
        KeyCode::ShiftLeft => Code::ShiftLeft,
        KeyCode::ShiftRight => Code::ShiftRight,
        KeyCode::ControlLeft => Code::ControlLeft,
        KeyCode::ControlRight => Code::ControlRight,
        KeyCode::AltLeft => Code::AltLeft,
        KeyCode::AltRight => Code::AltRight,
        KeyCode::SuperLeft => Code::MetaLeft,
        KeyCode::SuperRight => Code::MetaRight,
        _ => Code::Unidentified,
    }
}

pub fn keyboard_event(
    event: &KeyEvent,
    modifiers: ModifiersState,
    composing: bool,
) -> KeyboardEvent {
    let key = match &event.logical_key {
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(WinitNamedKey::Space) => Key::Character(" ".into()),
        WinitKey::Named(named) => Key::Named(named_key(named)),
        _ => Key::Named(NamedKey::Unidentified),
    };
    let mut flags = Modifiers::empty();
    flags.set(Modifiers::SHIFT, modifiers.shift_key());
    flags.set(Modifiers::CONTROL, modifiers.control_key());
    flags.set(Modifiers::ALT, modifiers.alt_key());
    flags.set(Modifiers::META, modifiers.super_key());
    KeyboardEvent::new_without_event(
        match event.state {
            ElementState::Pressed => KeyState::Down,
            ElementState::Released => KeyState::Up,
        },
        key,
        physical_code(event.physical_key),
        match event.location {
            KeyLocation::Standard => Location::Standard,
            KeyLocation::Left => Location::Left,
            KeyLocation::Right => Location::Right,
            KeyLocation::Numpad => Location::Numpad,
        },
        flags,
        event.repeat,
        composing,
    )
}

pub fn mouse_button(button: WinitMouseButton) -> servo::MouseButton {
    match button {
        WinitMouseButton::Left => servo::MouseButton::Primary,
        WinitMouseButton::Middle => servo::MouseButton::Auxiliary,
        WinitMouseButton::Right => servo::MouseButton::Secondary,
        WinitMouseButton::Back => servo::MouseButton::Back,
        WinitMouseButton::Forward => servo::MouseButton::Forward,
        WinitMouseButton::Other(button) => servo::MouseButton::Other(button),
    }
}

fn cursor_icon(cursor: Cursor) -> Option<CursorIcon> {
    Some(match cursor {
        Cursor::None => return None,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Move => CursorIcon::Move,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::EwResize | Cursor::ColResize => CursorIcon::EwResize,
        Cursor::NsResize | Cursor::RowResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        _ => CursorIcon::Default,
    })
}

pub fn set_cursor(window: &Window, cursor: Cursor) {
    match cursor_icon(cursor) {
        Some(icon) => {
            window.set_cursor(icon);
            window.set_cursor_visible(true);
        }
        None => window.set_cursor_visible(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::NativeKeyCode;

    #[test]
    fn maps_function_keys_f1_to_f24() {
        let cases = [
            (WinitNamedKey::F1, NamedKey::F1),
            (WinitNamedKey::F2, NamedKey::F2),
            (WinitNamedKey::F5, NamedKey::F5),
            (WinitNamedKey::F11, NamedKey::F11),
            (WinitNamedKey::F12, NamedKey::F12),
            (WinitNamedKey::F13, NamedKey::F13),
            (WinitNamedKey::F24, NamedKey::F24),
        ];
        for (input, expected) in cases {
            assert_eq!(named_key(&input), expected);
        }
    }

    #[test]
    fn maps_physical_codes_and_falls_back_to_unidentified() {
        let cases = [
            (PhysicalKey::Code(KeyCode::Backquote), Code::Backquote),
            (PhysicalKey::Code(KeyCode::Digit0), Code::Digit0),
            (PhysicalKey::Code(KeyCode::Digit9), Code::Digit9),
            (PhysicalKey::Code(KeyCode::KeyA), Code::KeyA),
            (PhysicalKey::Code(KeyCode::KeyZ), Code::KeyZ),
            (PhysicalKey::Code(KeyCode::Minus), Code::Minus),
            (PhysicalKey::Code(KeyCode::Equal), Code::Equal),
            (PhysicalKey::Code(KeyCode::BracketLeft), Code::BracketLeft),
            (PhysicalKey::Code(KeyCode::BracketRight), Code::BracketRight),
            (PhysicalKey::Code(KeyCode::Backslash), Code::Backslash),
            (PhysicalKey::Code(KeyCode::Semicolon), Code::Semicolon),
            (PhysicalKey::Code(KeyCode::Quote), Code::Quote),
            (PhysicalKey::Code(KeyCode::Comma), Code::Comma),
            (PhysicalKey::Code(KeyCode::Period), Code::Period),
            (PhysicalKey::Code(KeyCode::Slash), Code::Slash),
            (PhysicalKey::Code(KeyCode::Space), Code::Space),
            (PhysicalKey::Code(KeyCode::Enter), Code::Enter),
            (PhysicalKey::Code(KeyCode::Tab), Code::Tab),
            (PhysicalKey::Code(KeyCode::Backspace), Code::Backspace),
            (PhysicalKey::Code(KeyCode::Delete), Code::Delete),
            (PhysicalKey::Code(KeyCode::Insert), Code::Insert),
            (PhysicalKey::Code(KeyCode::Escape), Code::Escape),
            (PhysicalKey::Code(KeyCode::ArrowUp), Code::ArrowUp),
            (PhysicalKey::Code(KeyCode::ArrowDown), Code::ArrowDown),
            (PhysicalKey::Code(KeyCode::ArrowLeft), Code::ArrowLeft),
            (PhysicalKey::Code(KeyCode::ArrowRight), Code::ArrowRight),
            (PhysicalKey::Code(KeyCode::Home), Code::Home),
            (PhysicalKey::Code(KeyCode::End), Code::End),
            (PhysicalKey::Code(KeyCode::PageUp), Code::PageUp),
            (PhysicalKey::Code(KeyCode::PageDown), Code::PageDown),
            (PhysicalKey::Code(KeyCode::F1), Code::F1),
            (PhysicalKey::Code(KeyCode::F24), Code::F24),
            (PhysicalKey::Code(KeyCode::ShiftLeft), Code::ShiftLeft),
            (PhysicalKey::Code(KeyCode::ShiftRight), Code::ShiftRight),
            (PhysicalKey::Code(KeyCode::ControlLeft), Code::ControlLeft),
            (PhysicalKey::Code(KeyCode::ControlRight), Code::ControlRight),
            (PhysicalKey::Code(KeyCode::AltLeft), Code::AltLeft),
            (PhysicalKey::Code(KeyCode::AltRight), Code::AltRight),
            (PhysicalKey::Code(KeyCode::SuperLeft), Code::MetaLeft),
            (PhysicalKey::Code(KeyCode::SuperRight), Code::MetaRight),
            (
                PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                Code::Unidentified,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(physical_code(input), expected);
        }
    }

    #[test]
    fn maps_resize_and_corner_cursors() {
        assert_eq!(cursor_icon(Cursor::None), None);
        assert_eq!(cursor_icon(Cursor::NResize), Some(CursorIcon::NResize));
        assert_eq!(cursor_icon(Cursor::SResize), Some(CursorIcon::SResize));
        assert_eq!(cursor_icon(Cursor::EResize), Some(CursorIcon::EResize));
        assert_eq!(cursor_icon(Cursor::WResize), Some(CursorIcon::WResize));
        assert_eq!(cursor_icon(Cursor::NeResize), Some(CursorIcon::NeResize));
        assert_eq!(cursor_icon(Cursor::NwResize), Some(CursorIcon::NwResize));
        assert_eq!(cursor_icon(Cursor::SeResize), Some(CursorIcon::SeResize));
        assert_eq!(cursor_icon(Cursor::SwResize), Some(CursorIcon::SwResize));
        assert_eq!(
            cursor_icon(Cursor::NeswResize),
            Some(CursorIcon::NeswResize)
        );
        assert_eq!(
            cursor_icon(Cursor::NwseResize),
            Some(CursorIcon::NwseResize)
        );
        assert_eq!(cursor_icon(Cursor::ColResize), Some(CursorIcon::EwResize));
        assert_eq!(cursor_icon(Cursor::RowResize), Some(CursorIcon::NsResize));
    }
}
