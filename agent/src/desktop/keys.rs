//! Key names as clients write them: `enter`, `f5`, `ctrl+shift+s`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A key by the character it types unshifted: `a`, `7`, `/`.
    Char(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    Space,
    /// F1 to F24.
    F(u8),
    Ctrl,
    Shift,
    Alt,
    /// The Windows key, Super, or Command.
    Meta,
    CapsLock,
    PrintScreen,
    Menu,
}

impl Key {
    pub fn is_modifier(self) -> bool {
        matches!(self, Key::Ctrl | Key::Shift | Key::Alt | Key::Meta)
    }
}

fn parse_one(name: &str) -> Result<Key, String> {
    let lower = name.to_lowercase();
    Ok(match lower.as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "space" => Key::Space,
        "ctrl" | "control" => Key::Ctrl,
        "shift" => Key::Shift,
        "alt" | "option" => Key::Alt,
        "meta" | "win" | "windows" | "super" | "cmd" | "command" => Key::Meta,
        "capslock" => Key::CapsLock,
        "printscreen" | "prtsc" => Key::PrintScreen,
        "menu" => Key::Menu,
        "plus" => Key::Char('+'),
        _ => {
            if let Some(number) = lower.strip_prefix('f').and_then(|digits| digits.parse::<u8>().ok()) {
                if (1..=24).contains(&number) {
                    return Ok(Key::F(number));
                }
            }
            let mut characters = lower.chars();
            match (characters.next(), characters.next()) {
                (Some(character), None) if !character.is_control() => Key::Char(character),
                _ => return Err(format!("{name} isn't a key name")),
            }
        }
    })
}

/// Parses `ctrl+shift+s` into the keys to hold, in order: any modifiers, then one other key. A modifier alone
/// (`meta`) is a chord too.
pub fn parse_chord(text: &str) -> Result<Vec<Key>, String> {
    let names: Vec<&str> = text.split('+').map(str::trim).collect();
    if text.trim().is_empty() || names.iter().any(|name| name.is_empty()) {
        return Err(format!("\"{text}\" isn't a key chord; write it like ctrl+s, and the plus key as \"plus\""));
    }
    let keys = names.into_iter().map(parse_one).collect::<Result<Vec<_>, _>>()?;
    let (last, held) = keys.split_last().expect("at least one name");
    if let Some(wrong) = held.iter().find(|key| !key.is_modifier()) {
        return Err(format!("only the last key of a chord can be a non-modifier; {wrong:?} is not ctrl, shift, alt or meta"));
    }
    if held.contains(last) || held.iter().enumerate().any(|(index, key)| held[..index].contains(key)) {
        return Err(format!("\"{text}\" names a key twice"));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse() {
        assert_eq!(parse_chord("ctrl+s").unwrap(), [Key::Ctrl, Key::Char('s')]);
        assert_eq!(parse_chord("Ctrl + Shift + S").unwrap(), [Key::Ctrl, Key::Shift, Key::Char('s')]);
        assert_eq!(parse_chord("enter").unwrap(), [Key::Enter]);
        assert_eq!(parse_chord("alt+f4").unwrap(), [Key::Alt, Key::F(4)]);
        assert_eq!(parse_chord("win+ctrl+right").unwrap(), [Key::Meta, Key::Ctrl, Key::Right]);
        assert_eq!(parse_chord("ctrl+plus").unwrap(), [Key::Ctrl, Key::Char('+')]);
        assert_eq!(parse_chord("meta").unwrap(), [Key::Meta]);
        assert_eq!(parse_chord("é").unwrap(), [Key::Char('é')]);
        assert_eq!(parse_chord("f24").unwrap(), [Key::F(24)]);
    }

    #[test]
    fn mistakes_are_explained() {
        for wrong in ["", "ctrl+", "+s", "ctrl++", "a+b", "ctrl+ctrl", "ctrl+shift+ctrl+s", "f25", "f0", "enterr", "ctrl+ab"] {
            assert!(parse_chord(wrong).is_err(), "{wrong}");
        }
    }
}
