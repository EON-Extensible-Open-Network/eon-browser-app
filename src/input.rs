// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Keyboard and mouse from the window (winit) into what Servo understands. winit and
//! Servo both follow the W3C names for keys and key codes, so most of the mapping is by
//! name; the exceptions are written out.

use std::str::FromStr;

use servo::{Code, Key, KeyState, KeyboardEvent, Location, Modifiers, NamedKey};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{
    Key as WKey, KeyCode, KeyLocation, ModifiersState, NamedKey as WNamed, PhysicalKey,
};

/// winit's modifier state as Servo's.
pub fn modifiers(m: ModifiersState) -> Modifiers {
    let mut out = Modifiers::empty();
    if m.shift_key() {
        out |= Modifiers::SHIFT;
    }
    if m.control_key() {
        out |= Modifiers::CONTROL;
    }
    if m.alt_key() {
        out |= Modifiers::ALT;
    }
    if m.super_key() {
        out |= Modifiers::META;
    }
    out
}

/// A key press or release for Servo.
pub fn keyboard(event: &KeyEvent, mods: ModifiersState) -> KeyboardEvent {
    let state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };
    KeyboardEvent::new_without_event(
        state,
        key(&event.logical_key),
        code(event.physical_key),
        location(event.location),
        modifiers(mods),
        event.repeat,
        false,
    )
}

/// The logical key: the character typed, or the named key.
pub fn key(k: &WKey) -> Key {
    match k {
        WKey::Character(s) => Key::Character(s.to_string()),
        WKey::Named(WNamed::Space) => Key::Character(" ".into()),
        WKey::Named(WNamed::Super) => Key::Named(NamedKey::Meta),
        WKey::Named(n) => NamedKey::from_str(&format!("{n:?}"))
            .map_or(Key::Named(NamedKey::Unidentified), Key::Named),
        WKey::Unidentified(_) | WKey::Dead(_) => Key::Named(NamedKey::Unidentified),
    }
}

/// The physical key, by its W3C code name.
pub fn code(k: PhysicalKey) -> Code {
    match k {
        PhysicalKey::Code(KeyCode::SuperLeft) => Code::MetaLeft,
        PhysicalKey::Code(KeyCode::SuperRight) => Code::MetaRight,
        PhysicalKey::Code(c) => Code::from_str(&format!("{c:?}")).unwrap_or(Code::Unidentified),
        PhysicalKey::Unidentified(_) => Code::Unidentified,
    }
}

fn location(l: KeyLocation) -> Location {
    match l {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    }
}

/// The browser's own shortcuts, decided before a key reaches a page or the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    FocusAddress,
    Back,
    Forward,
    Reload,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewPrivateWindow,
    /// Tor Browser's shortcut: every circuit and every cookie renewed.
    NewIdentity,
    /// Add the page to bookmarks, or remove it.
    Bookmark,
    History,
    /// The command bar (`Ctrl+K`).
    Palette,
}

/// The shortcut a key press means, if any. Ctrl on Windows and Linux, Cmd on macOS.
pub fn shortcut(event: &KeyEvent, mods: ModifiersState) -> Option<Shortcut> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let primary = if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    };
    let shift = mods.shift_key();
    match (&event.logical_key, primary, mods.alt_key(), shift) {
        (WKey::Named(WNamed::F5), false, false, _) => Some(Shortcut::Reload),
        (WKey::Named(WNamed::F6), false, false, false) => Some(Shortcut::FocusAddress),
        (WKey::Named(WNamed::ArrowLeft), false, true, false)
        | (WKey::Named(WNamed::BrowserBack), ..) => Some(Shortcut::Back),
        (WKey::Named(WNamed::ArrowRight), false, true, false)
        | (WKey::Named(WNamed::BrowserForward), ..) => Some(Shortcut::Forward),
        (WKey::Named(WNamed::Tab), true, false, false) => Some(Shortcut::NextTab),
        (WKey::Named(WNamed::Tab), true, false, true) => Some(Shortcut::PreviousTab),
        (WKey::Character(c), true, false, _) => match c.to_lowercase().as_str() {
            "t" if !shift => Some(Shortcut::NewTab),
            "w" if !shift => Some(Shortcut::CloseTab),
            "l" if !shift => Some(Shortcut::FocusAddress),
            "r" => Some(Shortcut::Reload),
            "n" | "p" if shift => Some(Shortcut::NewPrivateWindow),
            "u" if shift => Some(Shortcut::NewIdentity),
            "d" if !shift => Some(Shortcut::Bookmark),
            "k" if !shift => Some(Shortcut::Palette),
            "h" if !shift => Some(Shortcut::History),
            "=" | "+" => Some(Shortcut::ZoomIn),
            "-" => Some(Shortcut::ZoomOut),
            "0" => Some(Shortcut::ZoomReset),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_by_w3c_name() {
        assert_eq!(
            key(&WKey::Character("ğ".into())),
            Key::Character("ğ".into())
        );
        assert_eq!(
            key(&WKey::Named(WNamed::Enter)),
            Key::Named(NamedKey::Enter)
        );
        assert_eq!(
            key(&WKey::Named(WNamed::ArrowLeft)),
            Key::Named(NamedKey::ArrowLeft)
        );
        assert_eq!(
            key(&WKey::Named(WNamed::Backspace)),
            Key::Named(NamedKey::Backspace)
        );
        assert_eq!(key(&WKey::Named(WNamed::Space)), Key::Character(" ".into()));
        assert_eq!(key(&WKey::Named(WNamed::Super)), Key::Named(NamedKey::Meta));
        assert_eq!(key(&WKey::Named(WNamed::F11)), Key::Named(NamedKey::F11));
    }

    #[test]
    fn codes_map_by_w3c_name() {
        assert_eq!(code(PhysicalKey::Code(KeyCode::KeyA)), Code::KeyA);
        assert_eq!(code(PhysicalKey::Code(KeyCode::Digit7)), Code::Digit7);
        assert_eq!(code(PhysicalKey::Code(KeyCode::ArrowDown)), Code::ArrowDown);
        assert_eq!(code(PhysicalKey::Code(KeyCode::ShiftLeft)), Code::ShiftLeft);
        assert_eq!(code(PhysicalKey::Code(KeyCode::SuperLeft)), Code::MetaLeft);
        assert_eq!(code(PhysicalKey::Code(KeyCode::Semicolon)), Code::Semicolon);
    }

    #[test]
    fn modifier_flags() {
        let m = modifiers(ModifiersState::SHIFT | ModifiersState::CONTROL);
        assert!(m.contains(Modifiers::SHIFT) && m.contains(Modifiers::CONTROL));
        assert!(!m.contains(Modifiers::ALT));
    }
}
