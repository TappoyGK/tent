use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalPosition<T> {
    pub x: T,
    pub y: T,
}

impl<T> LogicalPosition<T> {
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalSize<T> {
    pub width: T,
    pub height: T,
}

impl<T> LogicalSize<T> {
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalPosition<T> {
    pub x: T,
    pub y: T,
}

impl<T> PhysicalPosition<T> {
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl PhysicalPosition<i32> {
    pub fn to_logical(self, scale_factor: f64) -> LogicalPosition<i32> {
        let s = scale_factor.max(1.0);
        LogicalPosition::new(
            (f64::from(self.x) / s).round() as i32,
            (f64::from(self.y) / s).round() as i32,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhysicalSize<T> {
    pub width: T,
    pub height: T,
}

impl<T> PhysicalSize<T> {
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl PhysicalSize<u32> {
    pub fn to_logical(self, scale_factor: f64) -> LogicalSize<u32> {
        let s = scale_factor.max(1.0);
        LogicalSize::new(
            (f64::from(self.width) / s).round() as u32,
            (f64::from(self.height) / s).round() as u32,
        )
    }
}

/// 修飾キー状態（Shift / Control / Option(Alt) / Command(Super)）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifiersState {
    bits: u8,
}

impl ModifiersState {
    pub const SHIFT: Self = Self { bits: 1 << 0 };
    pub const CONTROL: Self = Self { bits: 1 << 1 };
    pub const ALT: Self = Self { bits: 1 << 2 };
    pub const SUPER: Self = Self { bits: 1 << 3 };

    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn shift_key(self) -> bool {
        (self.bits & Self::SHIFT.bits) != 0
    }

    pub const fn control_key(self) -> bool {
        (self.bits & Self::CONTROL.bits) != 0
    }

    pub const fn alt_key(self) -> bool {
        (self.bits & Self::ALT.bits) != 0
    }

    pub const fn super_key(self) -> bool {
        (self.bits & Self::SUPER.bits) != 0
    }

    /// AppKit `NSEventModifierFlags` のビットマスクから変換する
    pub const fn from_macos_flags(flags: usize) -> Self {
        let mut bits = 0u8;
        if (flags & (1 << 17)) != 0 {
            bits |= Self::SHIFT.bits;
        }
        if (flags & (1 << 18)) != 0 {
            bits |= Self::CONTROL.bits;
        }
        if (flags & (1 << 19)) != 0 {
            bits |= Self::ALT.bits;
        }
        if (flags & (1 << 20)) != 0 {
            bits |= Self::SUPER.bits;
        }
        Self { bits }
    }
}

impl std::ops::BitOr for ModifiersState {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self {
            bits: self.bits | rhs.bits,
        }
    }
}

impl std::ops::BitOrAssign for ModifiersState {
    fn bitor_assign(&mut self, rhs: Self) {
        self.bits |= rhs.bits;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseScrollDelta {
    LineDelta(f32, f32),
    PixelDelta(PhysicalPosition<f64>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ime {
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedKey {
    Escape,
    Enter,
    Backspace,
    Tab,
    Space,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    Character(String),
    Unidentified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    IntlBackslash,
    IntlRo,
    IntlYen,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    AltLeft,
    AltRight,
    Backspace,
    CapsLock,
    ControlLeft,
    ControlRight,
    Enter,
    SuperLeft,
    SuperRight,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    Delete,
    End,
    Home,
    Insert,
    PageDown,
    PageUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    NumpadEnter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalKey {
    Code(KeyCode),
    Unidentified,
}

#[derive(Clone, Debug)]
pub struct KeyEvent {
    pub physical_key: PhysicalKey,
    pub logical_key: Key,
    pub unshifted_char: Option<char>,
    pub state: ElementState,
    pub repeat: bool,
    pub text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlFlow {
    Wait,
    WaitUntil(Instant),
}

#[derive(Clone, Debug)]
pub enum WindowEvent {
    CloseRequested,
    Focused(bool),
    Resized,
    ScaleFactorChanged,
    ModifiersChanged(ModifiersState),
    CursorMoved {
        position: PhysicalPosition<f64>,
    },
    MouseInput {
        state: ElementState,
        button: MouseButton,
    },
    MouseWheel {
        delta: MouseScrollDelta,
    },
    KeyboardInput {
        event: KeyEvent,
    },
    Ime(Ime),
    RedrawRequested,
}

/// macOS Carbon/HID 仮想キーコード (`NSEvent.keyCode`) を `PhysicalKey` へ変換する
pub fn macos_keycode_to_physical(keycode: u16) -> PhysicalKey {
    let code = match keycode {
        0x00 => KeyCode::KeyA,
        0x01 => KeyCode::KeyS,
        0x02 => KeyCode::KeyD,
        0x03 => KeyCode::KeyF,
        0x04 => KeyCode::KeyH,
        0x05 => KeyCode::KeyG,
        0x06 => KeyCode::KeyZ,
        0x07 => KeyCode::KeyX,
        0x08 => KeyCode::KeyC,
        0x09 => KeyCode::KeyV,
        0x0a => KeyCode::IntlBackslash,
        0x0b => KeyCode::KeyB,
        0x0c => KeyCode::KeyQ,
        0x0d => KeyCode::KeyW,
        0x0e => KeyCode::KeyE,
        0x0f => KeyCode::KeyR,
        0x10 => KeyCode::KeyY,
        0x11 => KeyCode::KeyT,
        0x12 => KeyCode::Digit1,
        0x13 => KeyCode::Digit2,
        0x14 => KeyCode::Digit3,
        0x15 => KeyCode::Digit4,
        0x16 => KeyCode::Digit6,
        0x17 => KeyCode::Digit5,
        0x18 => KeyCode::Equal,
        0x19 => KeyCode::Digit9,
        0x1a => KeyCode::Digit7,
        0x1b => KeyCode::Minus,
        0x1c => KeyCode::Digit8,
        0x1d => KeyCode::Digit0,
        0x1e => KeyCode::BracketRight,
        0x1f => KeyCode::KeyO,
        0x20 => KeyCode::KeyU,
        0x21 => KeyCode::BracketLeft,
        0x22 => KeyCode::KeyI,
        0x23 => KeyCode::KeyP,
        0x24 => KeyCode::Enter,
        0x25 => KeyCode::KeyL,
        0x26 => KeyCode::KeyJ,
        0x27 => KeyCode::Quote,
        0x28 => KeyCode::KeyK,
        0x29 => KeyCode::Semicolon,
        0x2a => KeyCode::Backslash,
        0x2b => KeyCode::Comma,
        0x2c => KeyCode::Slash,
        0x2d => KeyCode::KeyN,
        0x2e => KeyCode::KeyM,
        0x2f => KeyCode::Period,
        0x30 => KeyCode::Tab,
        0x31 => KeyCode::Space,
        0x32 => KeyCode::Backquote,
        0x33 => KeyCode::Backspace,
        0x35 => KeyCode::Escape,
        0x36 => KeyCode::SuperRight,
        0x37 => KeyCode::SuperLeft,
        0x38 => KeyCode::ShiftLeft,
        0x39 => KeyCode::CapsLock,
        0x3a => KeyCode::AltLeft,
        0x3b => KeyCode::ControlLeft,
        0x3c => KeyCode::ShiftRight,
        0x3d => KeyCode::AltRight,
        0x3e => KeyCode::ControlRight,
        0x4c => KeyCode::NumpadEnter,
        0x5d => KeyCode::IntlYen,
        0x5e => KeyCode::IntlRo,
        0x60 => KeyCode::F5,
        0x61 => KeyCode::F6,
        0x62 => KeyCode::F7,
        0x63 => KeyCode::F3,
        0x64 => KeyCode::F8,
        0x65 => KeyCode::F9,
        0x67 => KeyCode::F11,
        0x6d => KeyCode::F10,
        0x6f => KeyCode::F12,
        0x72 => KeyCode::Insert,
        0x73 => KeyCode::Home,
        0x74 => KeyCode::PageUp,
        0x75 => KeyCode::Delete,
        0x76 => KeyCode::F4,
        0x77 => KeyCode::End,
        0x78 => KeyCode::F2,
        0x79 => KeyCode::PageDown,
        0x7a => KeyCode::F1,
        0x7b => KeyCode::ArrowLeft,
        0x7c => KeyCode::ArrowRight,
        0x7d => KeyCode::ArrowDown,
        0x7e => KeyCode::ArrowUp,
        _ => return PhysicalKey::Unidentified,
    };
    PhysicalKey::Code(code)
}

/// 物理キーコードから US 配列ベースの基本文字（unshifted）を引くフォールバック
pub fn fallback_unshifted_char(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::KeyA => Some('a'),
        KeyCode::KeyB => Some('b'),
        KeyCode::KeyC => Some('c'),
        KeyCode::KeyD => Some('d'),
        KeyCode::KeyE => Some('e'),
        KeyCode::KeyF => Some('f'),
        KeyCode::KeyG => Some('g'),
        KeyCode::KeyH => Some('h'),
        KeyCode::KeyI => Some('i'),
        KeyCode::KeyJ => Some('j'),
        KeyCode::KeyK => Some('k'),
        KeyCode::KeyL => Some('l'),
        KeyCode::KeyM => Some('m'),
        KeyCode::KeyN => Some('n'),
        KeyCode::KeyO => Some('o'),
        KeyCode::KeyP => Some('p'),
        KeyCode::KeyQ => Some('q'),
        KeyCode::KeyR => Some('r'),
        KeyCode::KeyS => Some('s'),
        KeyCode::KeyT => Some('t'),
        KeyCode::KeyU => Some('u'),
        KeyCode::KeyV => Some('v'),
        KeyCode::KeyW => Some('w'),
        KeyCode::KeyX => Some('x'),
        KeyCode::KeyY => Some('y'),
        KeyCode::KeyZ => Some('z'),
        KeyCode::Digit0 => Some('0'),
        KeyCode::Digit1 => Some('1'),
        KeyCode::Digit2 => Some('2'),
        KeyCode::Digit3 => Some('3'),
        KeyCode::Digit4 => Some('4'),
        KeyCode::Digit5 => Some('5'),
        KeyCode::Digit6 => Some('6'),
        KeyCode::Digit7 => Some('7'),
        KeyCode::Digit8 => Some('8'),
        KeyCode::Digit9 => Some('9'),
        KeyCode::Minus => Some('-'),
        KeyCode::Equal => Some('='),
        KeyCode::BracketLeft => Some('['),
        KeyCode::BracketRight => Some(']'),
        KeyCode::Backslash => Some('\\'),
        KeyCode::Semicolon => Some(';'),
        KeyCode::Quote => Some('\''),
        KeyCode::Backquote => Some('`'),
        KeyCode::Comma => Some(','),
        KeyCode::Period => Some('.'),
        KeyCode::Slash => Some('/'),
        KeyCode::Space => Some(' '),
        _ => None,
    }
}

/// 物理キーコードから特殊キーの `NamedKey` を引く
pub fn physical_to_named_key(code: KeyCode) -> Option<NamedKey> {
    match code {
        KeyCode::Escape => Some(NamedKey::Escape),
        KeyCode::Enter | KeyCode::NumpadEnter => Some(NamedKey::Enter),
        KeyCode::Backspace => Some(NamedKey::Backspace),
        KeyCode::Tab => Some(NamedKey::Tab),
        KeyCode::Space => Some(NamedKey::Space),
        KeyCode::Delete => Some(NamedKey::Delete),
        KeyCode::Home => Some(NamedKey::Home),
        KeyCode::End => Some(NamedKey::End),
        KeyCode::PageUp => Some(NamedKey::PageUp),
        KeyCode::PageDown => Some(NamedKey::PageDown),
        KeyCode::ArrowUp => Some(NamedKey::ArrowUp),
        KeyCode::ArrowDown => Some(NamedKey::ArrowDown),
        KeyCode::ArrowLeft => Some(NamedKey::ArrowLeft),
        KeyCode::ArrowRight => Some(NamedKey::ArrowRight),
        KeyCode::F1 => Some(NamedKey::F1),
        KeyCode::F2 => Some(NamedKey::F2),
        KeyCode::F3 => Some(NamedKey::F3),
        KeyCode::F4 => Some(NamedKey::F4),
        KeyCode::F5 => Some(NamedKey::F5),
        KeyCode::F6 => Some(NamedKey::F6),
        KeyCode::F7 => Some(NamedKey::F7),
        KeyCode::F8 => Some(NamedKey::F8),
        KeyCode::F9 => Some(NamedKey::F9),
        KeyCode::F10 => Some(NamedKey::F10),
        KeyCode::F11 => Some(NamedKey::F11),
        KeyCode::F12 => Some(NamedKey::F12),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_keycode_mapping() {
        assert_eq!(
            macos_keycode_to_physical(0x00),
            PhysicalKey::Code(KeyCode::KeyA)
        );
        assert_eq!(
            macos_keycode_to_physical(0x24),
            PhysicalKey::Code(KeyCode::Enter)
        );
        assert_eq!(
            macos_keycode_to_physical(0x35),
            PhysicalKey::Code(KeyCode::Escape)
        );
        assert_eq!(
            macos_keycode_to_physical(0x7b),
            PhysicalKey::Code(KeyCode::ArrowLeft)
        );
        assert_eq!(
            macos_keycode_to_physical(0x7c),
            PhysicalKey::Code(KeyCode::ArrowRight)
        );
        assert_eq!(
            macos_keycode_to_physical(0x7d),
            PhysicalKey::Code(KeyCode::ArrowDown)
        );
        assert_eq!(
            macos_keycode_to_physical(0x7e),
            PhysicalKey::Code(KeyCode::ArrowUp)
        );
    }

    #[test]
    fn test_modifiers_from_macos_flags() {
        let m = ModifiersState::from_macos_flags((1 << 17) | (1 << 20));
        assert!(m.shift_key());
        assert!(m.super_key());
        assert!(!m.control_key());
        assert!(!m.alt_key());
    }
}
