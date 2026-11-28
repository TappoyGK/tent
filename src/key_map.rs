use ghostty_vt::Terminal;
use ghostty_vt::key::{Action, Encoder, Event as VtKeyEvent, Key as VtKey, Mods, OptionAsAlt};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// ペインフォーカス移動方向
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDir {
    Left,
    Right,
    Up,
    Down,
    Prev,
    Next,
}

/// TenT のアプリ側ショートカット
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppShortcut {
    SelectSlot(usize),
    ToggleCol(usize),
    ToggleAllCols,
    SplitVertical,   // Cmd+D (左右分割: iTerm2互換)
    SplitHorizontal, // Cmd+Shift+D (上下分割: iTerm2互換)
    ClosePane,       // Cmd+W
    FocusPane(FocusDir),
    ResizePane(FocusDir), // Cmd+Ctrl+Arrow (分割ペイン境界リサイズ)
    ToggleBrowser,        // Cmd+B (スロットのターミナル⇔ブラウザ切替)
    SplitBrowser,         // Cmd+Shift+B (ブラウザペインを左右分割で追加)
    FocusUrlBar,          // Cmd+L (URLバーにフォーカス)
    ReloadBrowser,        // Cmd+R (ブラウザ再読み込み)
    BrowserBack,          // Cmd+Left / スワイプ戻る
    BrowserForward,       // Cmd+Right / スワイプ進む
    RestartApp,           // Cmd+Opt+R (TenT 本体再起動)
    ToggleHelp,           // Cmd+/ / Cmd+? (ショートカットヘルプ)
    Copy,                 // Cmd+C
    Paste,                // Cmd+V
    ClearScreen,          // Cmd+K (画面・スクロールバッククリア)
    Find,                 // Cmd+F (ペイン内検索)
}

/// Cmd+1..9/0、Cmd+Opt+1/2、Cmd+\、iTerm2互換ペイン操作、ブラウザ操作、Cmd+C/V を判定する
pub fn match_app_shortcut(event: &KeyEvent, mods: ModifiersState) -> Option<AppShortcut> {
    if event.state != ElementState::Pressed {
        return None;
    }
    if !mods.super_key() {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };

    if mods.control_key() {
        if mods.shift_key() || mods.alt_key() {
            return None;
        }
        return match code {
            KeyCode::ArrowLeft => Some(AppShortcut::ResizePane(FocusDir::Left)),
            KeyCode::ArrowRight => Some(AppShortcut::ResizePane(FocusDir::Right)),
            KeyCode::ArrowUp => Some(AppShortcut::ResizePane(FocusDir::Up)),
            KeyCode::ArrowDown => Some(AppShortcut::ResizePane(FocusDir::Down)),
            _ => None,
        };
    }

    if mods.shift_key() && !mods.alt_key() {
        return match code {
            KeyCode::KeyD => Some(AppShortcut::SplitHorizontal),
            KeyCode::KeyB => Some(AppShortcut::SplitBrowser),
            KeyCode::Slash => Some(AppShortcut::ToggleHelp),
            _ => None,
        };
    }

    if mods.alt_key() && !mods.shift_key() {
        return match code {
            KeyCode::Digit1 => Some(AppShortcut::ToggleCol(0)),
            KeyCode::Digit2 => Some(AppShortcut::ToggleCol(1)),
            KeyCode::KeyR => Some(AppShortcut::RestartApp),
            KeyCode::ArrowLeft => Some(AppShortcut::FocusPane(FocusDir::Left)),
            KeyCode::ArrowRight => Some(AppShortcut::FocusPane(FocusDir::Right)),
            KeyCode::ArrowUp => Some(AppShortcut::FocusPane(FocusDir::Up)),
            KeyCode::ArrowDown => Some(AppShortcut::FocusPane(FocusDir::Down)),
            _ => None,
        };
    }

    if mods.shift_key() || mods.alt_key() {
        return None;
    }

    match code {
        KeyCode::Digit1 => Some(AppShortcut::SelectSlot(0)),
        KeyCode::Digit2 => Some(AppShortcut::SelectSlot(1)),
        KeyCode::Digit3 => Some(AppShortcut::SelectSlot(2)),
        KeyCode::Digit4 => Some(AppShortcut::SelectSlot(3)),
        KeyCode::Digit5 => Some(AppShortcut::SelectSlot(4)),
        KeyCode::Digit6 => Some(AppShortcut::SelectSlot(5)),
        KeyCode::Digit7 => Some(AppShortcut::SelectSlot(6)),
        KeyCode::Digit8 => Some(AppShortcut::SelectSlot(7)),
        KeyCode::Digit9 => Some(AppShortcut::SelectSlot(8)),
        KeyCode::Digit0 => Some(AppShortcut::SelectSlot(9)),
        KeyCode::Backslash => Some(AppShortcut::ToggleAllCols),
        KeyCode::KeyD => Some(AppShortcut::SplitVertical),
        KeyCode::KeyW => Some(AppShortcut::ClosePane),
        KeyCode::KeyB => Some(AppShortcut::ToggleBrowser),
        KeyCode::KeyL => Some(AppShortcut::FocusUrlBar),
        KeyCode::KeyR => Some(AppShortcut::ReloadBrowser),
        KeyCode::ArrowLeft => Some(AppShortcut::BrowserBack),
        KeyCode::ArrowRight => Some(AppShortcut::BrowserForward),
        KeyCode::Slash => Some(AppShortcut::ToggleHelp),
        KeyCode::BracketLeft => Some(AppShortcut::FocusPane(FocusDir::Prev)),
        KeyCode::BracketRight => Some(AppShortcut::FocusPane(FocusDir::Next)),
        KeyCode::KeyC => Some(AppShortcut::Copy),
        KeyCode::KeyV => Some(AppShortcut::Paste),
        KeyCode::KeyK => Some(AppShortcut::ClearScreen),
        KeyCode::KeyF => Some(AppShortcut::Find),
        _ => None,
    }
}

/// winit の KeyEvent に依存しない純粋なキー入力情報（ユニットテスト可能）
pub struct KeyInput<'a> {
    pub action: Action,
    pub key: VtKey,
    pub mods: Mods,
    pub unshifted_char: Option<char>,
    /// 現在のOSキーボード配列でShiftを反映した文字（Ctrl/Option変換前）
    pub shifted_char: Option<char>,
    pub text: Option<&'a str>,
}

/// winit のキーイベントを ghostty_vt::key::Encoder でエスケープシーケンスに変換する
pub fn encode_key_event(
    encoder: &mut Encoder,
    terminal: &Terminal,
    event: &KeyEvent,
    mods: ModifiersState,
) -> Vec<u8> {
    let action = match event.state {
        ElementState::Pressed => {
            if event.repeat {
                Action::Repeat
            } else {
                Action::Press
            }
        }
        ElementState::Released => return Vec::new(),
    };

    let unshifted_char = match event.key_without_modifiers() {
        Key::Character(ref s) => s.chars().next(),
        _ => None,
    };

    // winit の logical_key は macOS の charactersIgnoringModifiers に由来するため、
    // Ctrl/Option 併用時でも現在のOSキーボード配列（JIS/US等）における Shift 適用後文字を保持する
    let shifted_char = if mods.shift_key() {
        match &event.logical_key {
            Key::Character(s) => s.chars().next().filter(|&c| {
                let cp = c as u32;
                cp >= 0x20 && cp != 0x7f && (!mods.alt_key() || c.is_ascii())
            }),
            _ => None,
        }
    } else {
        None
    };

    let input = KeyInput {
        action,
        key: map_physical_key(event.physical_key),
        mods: map_mods(mods),
        unshifted_char,
        shifted_char,
        text: event.text.as_deref(),
    };
    encode_key_input(encoder, terminal, &input)
}

/// 純粋な KeyInput からエスケープシーケンスを生成する本体ロジック
pub fn encode_key_input(
    encoder: &mut Encoder,
    terminal: &Terminal,
    input: &KeyInput<'_>,
) -> Vec<u8> {
    // Cmd 単独のアプリショートカット系はPTYへ送らない
    if input.mods.contains(Mods::SUPER) {
        return Vec::new();
    }

    let kitty_enabled = terminal
        .kitty_keyboard_flags()
        .map(|f| !f.is_empty())
        .unwrap_or(false);

    // Kitty Keyboard Protocol が無効な通常モードでは、Ctrl+[ (=0x1b) や Ctrl+Shift+- (=0x1f) を
    // 確実に標準の C0 制御コードとして返す
    let ctrl = input.mods.contains(Mods::CTRL);
    let alt = input.mods.contains(Mods::ALT);
    let shift = input.mods.contains(Mods::SHIFT);

    if !kitty_enabled && ctrl && !alt {
        if !shift && input.unshifted_char == Some('[') {
            return vec![0x1b];
        }
        if shift && input.unshifted_char == Some('2') {
            return vec![0x00];
        }
        if shift && input.unshifted_char == Some('-') {
            return vec![0x1f];
        }
    }

    encoder.set_options_from_terminal(terminal);
    encoder.set_macos_option_as_alt(OptionAsAlt::True);

    let Ok(mut vt_event) = VtKeyEvent::new() else {
        return Vec::new();
    };
    vt_event.set_action(input.action);
    vt_event.set_key(input.key);
    vt_event.set_mods(input.mods);

    if let Some(ch) = input.unshifted_char {
        vt_event.set_unshifted_codepoint(ch);
    }

    // ghostty-vt の仕様:
    // - Ctrl / Alt 押下時は C0 制御文字や macOS Option 生成文字 ("ƒ"/"∫") を渡さず、
    //   Shift を反映した（ただし Ctrl/Alt 変換前の）文字を utf8 に渡す。
    // - Alt+Shift+, (M-<) 等では Shift が文字生成に消費されたことを consumed_mods に伝える。
    if ctrl || alt {
        if let Some(ch) = input.unshifted_char {
            let effective = if shift {
                input.shifted_char.unwrap_or_else(|| shifted_us_char(ch))
            } else {
                ch
            };
            if (effective as u32) >= 0x20 && (effective as u32) != 0x7f {
                vt_event.set_utf8(Some(effective.to_string()));
                if shift && effective != ch && alt && !ctrl {
                    vt_event.set_consumed_mods(Mods::SHIFT);
                }
            }
            if ctrl
                && !alt
                && let Some(norm_key) = ascii_char_to_vt_key(ch)
            {
                vt_event.set_key(norm_key);
            }
        }
    } else if let Some(text) = input.text {
        let valid_text = !text.is_empty()
            && !text.chars().all(|c| {
                let cp = c as u32;
                cp < 0x20 || cp == 0x7f || (0xF700..=0xF8FF).contains(&cp)
            });
        if valid_text {
            vt_event.set_utf8(Some(text));
            if shift && input.unshifted_char.map(|c| c.to_string()).as_deref() != Some(text) {
                vt_event.set_consumed_mods(Mods::SHIFT);
            }
        }
    }

    let mut out = Vec::new();
    if encoder.encode_to_vec(&vt_event, &mut out).is_ok() {
        out
    } else {
        Vec::new()
    }
}

/// US 配列で Shift 押下時に生成される文字を返す（Alt+Shift+, -> '<' などのため）
fn shifted_us_char(ch: char) -> char {
    match ch {
        'a'..='z' => ch.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        other => other,
    }
}

fn ascii_char_to_vt_key(ch: char) -> Option<VtKey> {
    match ch.to_ascii_lowercase() {
        'a' => Some(VtKey::A),
        'b' => Some(VtKey::B),
        'c' => Some(VtKey::C),
        'd' => Some(VtKey::D),
        'e' => Some(VtKey::E),
        'f' => Some(VtKey::F),
        'g' => Some(VtKey::G),
        'h' => Some(VtKey::H),
        'i' => Some(VtKey::I),
        'j' => Some(VtKey::J),
        'k' => Some(VtKey::K),
        'l' => Some(VtKey::L),
        'm' => Some(VtKey::M),
        'n' => Some(VtKey::N),
        'o' => Some(VtKey::O),
        'p' => Some(VtKey::P),
        'q' => Some(VtKey::Q),
        'r' => Some(VtKey::R),
        's' => Some(VtKey::S),
        't' => Some(VtKey::T),
        'u' => Some(VtKey::U),
        'v' => Some(VtKey::V),
        'w' => Some(VtKey::W),
        'x' => Some(VtKey::X),
        'y' => Some(VtKey::Y),
        'z' => Some(VtKey::Z),
        '[' => Some(VtKey::BracketLeft),
        ']' => Some(VtKey::BracketRight),
        '\\' => Some(VtKey::Backslash),
        '^' => Some(VtKey::Digit6),
        '-' | '_' => Some(VtKey::Minus),
        '/' => Some(VtKey::Slash),
        '@' => Some(VtKey::Digit2),
        ' ' => Some(VtKey::Space),
        _ => None,
    }
}

fn map_mods(mods: ModifiersState) -> Mods {
    let mut m = Mods::empty();
    if mods.shift_key() {
        m |= Mods::SHIFT;
    }
    if mods.control_key() {
        m |= Mods::CTRL;
    }
    if mods.alt_key() {
        m |= Mods::ALT;
    }
    if mods.super_key() {
        m |= Mods::SUPER;
    }
    m
}

fn map_physical_key(key: PhysicalKey) -> VtKey {
    let PhysicalKey::Code(code) = key else {
        return VtKey::Unidentified;
    };
    match code {
        KeyCode::Backquote => VtKey::Backquote,
        KeyCode::Backslash => VtKey::Backslash,
        KeyCode::BracketLeft => VtKey::BracketLeft,
        KeyCode::BracketRight => VtKey::BracketRight,
        KeyCode::Comma => VtKey::Comma,
        KeyCode::Digit0 => VtKey::Digit0,
        KeyCode::Digit1 => VtKey::Digit1,
        KeyCode::Digit2 => VtKey::Digit2,
        KeyCode::Digit3 => VtKey::Digit3,
        KeyCode::Digit4 => VtKey::Digit4,
        KeyCode::Digit5 => VtKey::Digit5,
        KeyCode::Digit6 => VtKey::Digit6,
        KeyCode::Digit7 => VtKey::Digit7,
        KeyCode::Digit8 => VtKey::Digit8,
        KeyCode::Digit9 => VtKey::Digit9,
        KeyCode::Equal => VtKey::Equal,
        KeyCode::IntlBackslash => VtKey::IntlBackslash,
        KeyCode::IntlRo => VtKey::IntlRo,
        KeyCode::IntlYen => VtKey::IntlYen,
        KeyCode::KeyA => VtKey::A,
        KeyCode::KeyB => VtKey::B,
        KeyCode::KeyC => VtKey::C,
        KeyCode::KeyD => VtKey::D,
        KeyCode::KeyE => VtKey::E,
        KeyCode::KeyF => VtKey::F,
        KeyCode::KeyG => VtKey::G,
        KeyCode::KeyH => VtKey::H,
        KeyCode::KeyI => VtKey::I,
        KeyCode::KeyJ => VtKey::J,
        KeyCode::KeyK => VtKey::K,
        KeyCode::KeyL => VtKey::L,
        KeyCode::KeyM => VtKey::M,
        KeyCode::KeyN => VtKey::N,
        KeyCode::KeyO => VtKey::O,
        KeyCode::KeyP => VtKey::P,
        KeyCode::KeyQ => VtKey::Q,
        KeyCode::KeyR => VtKey::R,
        KeyCode::KeyS => VtKey::S,
        KeyCode::KeyT => VtKey::T,
        KeyCode::KeyU => VtKey::U,
        KeyCode::KeyV => VtKey::V,
        KeyCode::KeyW => VtKey::W,
        KeyCode::KeyX => VtKey::X,
        KeyCode::KeyY => VtKey::Y,
        KeyCode::KeyZ => VtKey::Z,
        KeyCode::Minus => VtKey::Minus,
        KeyCode::Period => VtKey::Period,
        KeyCode::Quote => VtKey::Quote,
        KeyCode::Semicolon => VtKey::Semicolon,
        KeyCode::Slash => VtKey::Slash,
        KeyCode::AltLeft => VtKey::AltLeft,
        KeyCode::AltRight => VtKey::AltRight,
        KeyCode::Backspace => VtKey::Backspace,
        KeyCode::CapsLock => VtKey::CapsLock,
        KeyCode::ContextMenu => VtKey::ContextMenu,
        KeyCode::ControlLeft => VtKey::ControlLeft,
        KeyCode::ControlRight => VtKey::ControlRight,
        KeyCode::Enter => VtKey::Enter,
        KeyCode::SuperLeft => VtKey::MetaLeft,
        KeyCode::SuperRight => VtKey::MetaRight,
        KeyCode::ShiftLeft => VtKey::ShiftLeft,
        KeyCode::ShiftRight => VtKey::ShiftRight,
        KeyCode::Space => VtKey::Space,
        KeyCode::Tab => VtKey::Tab,
        KeyCode::Delete => VtKey::Delete,
        KeyCode::End => VtKey::End,
        KeyCode::Home => VtKey::Home,
        KeyCode::Insert => VtKey::Insert,
        KeyCode::PageDown => VtKey::PageDown,
        KeyCode::PageUp => VtKey::PageUp,
        KeyCode::ArrowDown => VtKey::ArrowDown,
        KeyCode::ArrowLeft => VtKey::ArrowLeft,
        KeyCode::ArrowRight => VtKey::ArrowRight,
        KeyCode::ArrowUp => VtKey::ArrowUp,
        KeyCode::Escape => VtKey::Escape,
        KeyCode::F1 => VtKey::F1,
        KeyCode::F2 => VtKey::F2,
        KeyCode::F3 => VtKey::F3,
        KeyCode::F4 => VtKey::F4,
        KeyCode::F5 => VtKey::F5,
        KeyCode::F6 => VtKey::F6,
        KeyCode::F7 => VtKey::F7,
        KeyCode::F8 => VtKey::F8,
        KeyCode::F9 => VtKey::F9,
        KeyCode::F10 => VtKey::F10,
        KeyCode::F11 => VtKey::F11,
        KeyCode::F12 => VtKey::F12,
        KeyCode::NumpadEnter => VtKey::NumpadEnter,
        _ => VtKey::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostty_vt::TerminalOptions;

    fn run_encode(
        key: VtKey,
        mods: Mods,
        unshifted_char: Option<char>,
        text: Option<&str>,
    ) -> Vec<u8> {
        let term = Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            ..Default::default()
        })
        .unwrap();
        let mut enc = Encoder::new().unwrap();
        let input = KeyInput {
            action: Action::Press,
            key,
            mods,
            unshifted_char,
            shifted_char: None,
            text,
        };
        encode_key_input(&mut enc, &term, &input)
    }

    fn run_encode_with_shifted(
        key: VtKey,
        mods: Mods,
        unshifted_char: Option<char>,
        shifted_char: Option<char>,
        text: Option<&str>,
    ) -> Vec<u8> {
        let term = Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            ..Default::default()
        })
        .unwrap();
        let mut enc = Encoder::new().unwrap();
        let input = KeyInput {
            action: Action::Press,
            key,
            mods,
            unshifted_char,
            shifted_char,
            text,
        };
        encode_key_input(&mut enc, &term, &input)
    }

    #[test]
    fn test_ctrl_sequences_through_encode_key_input() {
        assert_eq!(
            run_encode(VtKey::BracketLeft, Mods::CTRL, Some('['), Some("\x1b")),
            vec![0x1b]
        );
        assert_eq!(
            run_encode(VtKey::C, Mods::CTRL, Some('c'), Some("\x03")),
            vec![0x03]
        );
        assert_eq!(
            run_encode(VtKey::D, Mods::CTRL, Some('d'), Some("\x04")),
            vec![0x04]
        );
        assert_eq!(
            run_encode(VtKey::L, Mods::CTRL, Some('l'), Some("\x0c")),
            vec![0x0c]
        );
        assert_eq!(
            run_encode(VtKey::BracketRight, Mods::CTRL, Some(']'), Some("\x1d")),
            vec![0x1d]
        );
        assert_eq!(
            run_encode(VtKey::Backslash, Mods::CTRL, Some('\\'), Some("\x1c")),
            vec![0x1c]
        );
        assert_eq!(
            run_encode(VtKey::Digit2, Mods::CTRL | Mods::SHIFT, Some('2'), None),
            vec![0x00]
        );
        assert_eq!(run_encode(VtKey::Space, Mods::CTRL, None, None), vec![0x00]);
        assert_eq!(
            run_encode(VtKey::Digit6, Mods::CTRL | Mods::SHIFT, Some('6'), None),
            vec![0x1e]
        );
        assert_eq!(
            run_encode(VtKey::Minus, Mods::CTRL | Mods::SHIFT, Some('-'), None),
            vec![0x1f]
        );
    }

    #[test]
    fn test_shift_characters_through_encode_key_input() {
        for (key, unshifted, shifted) in [
            (VtKey::Semicolon, ';', ":"),
            (VtKey::A, 'a', "A"),
            (VtKey::Digit1, '1', "!"),
            (VtKey::Slash, '/', "?"),
            (VtKey::Quote, '\'', "\""),
            (VtKey::BracketLeft, '[', "{"),
        ] {
            let out = run_encode(key, Mods::SHIFT, Some(unshifted), Some(shifted));
            assert_eq!(out, shifted.as_bytes(), "failed for shift+{unshifted}");
        }
    }

    #[test]
    fn test_alt_and_special_keys_through_encode_key_input() {
        assert_eq!(
            run_encode(VtKey::B, Mods::ALT, Some('b'), Some("∫")),
            b"\x1bb"
        );
        assert_eq!(
            run_encode(VtKey::F, Mods::ALT, Some('f'), Some("ƒ")),
            b"\x1bf"
        );
        assert_eq!(
            run_encode(VtKey::D, Mods::ALT, Some('d'), Some("∂")),
            b"\x1bd"
        );
        assert_eq!(
            run_encode(VtKey::Backspace, Mods::ALT, None, None),
            b"\x1b\x7f"
        );
        // Alt+Shift+, (Emacs M-<)
        assert_eq!(
            run_encode(VtKey::Comma, Mods::ALT | Mods::SHIFT, Some(','), Some("¯")),
            b"\x1b<"
        );
        // Alt+Shift+. (Emacs M->)
        assert_eq!(
            run_encode(VtKey::Period, Mods::ALT | Mods::SHIFT, Some('.'), Some("˘")),
            b"\x1b>"
        );

        assert_eq!(run_encode(VtKey::Enter, Mods::empty(), None, None), b"\r");
        assert_eq!(
            run_encode(VtKey::Backspace, Mods::empty(), None, None),
            b"\x7f"
        );
        assert_eq!(run_encode(VtKey::Tab, Mods::empty(), None, None), b"\t");
        assert_eq!(run_encode(VtKey::Tab, Mods::SHIFT, None, None), b"\x1b[Z");
        assert_eq!(
            run_encode(VtKey::Escape, Mods::empty(), None, None),
            b"\x1b"
        );
        assert_eq!(
            run_encode(VtKey::ArrowUp, Mods::empty(), None, None),
            b"\x1b[A"
        );
        assert_eq!(
            run_encode(VtKey::ArrowRight, Mods::CTRL, None, None),
            b"\x1b[1;5C"
        );
        assert_eq!(
            run_encode(VtKey::ArrowLeft, Mods::CTRL, None, None),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn test_layout_aware_shifted_char_for_us_and_jis() {
        // US配列: Option+Shift+2 -> M-@
        assert_eq!(
            run_encode_with_shifted(
                VtKey::Digit2,
                Mods::ALT | Mods::SHIFT,
                Some('2'),
                Some('@'),
                None
            ),
            b"\x1b@"
        );
        // JIS配列: Option+Shift+2 -> M-"
        assert_eq!(
            run_encode_with_shifted(
                VtKey::Digit2,
                Mods::ALT | Mods::SHIFT,
                Some('2'),
                Some('"'),
                None
            ),
            b"\x1b\""
        );
    }
}
