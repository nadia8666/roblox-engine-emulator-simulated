//! Keys, from the terminal or from a key script (`render --keys "2 j j enter"`), so a screenshot can be taken at any
//! state the keyboard reaches.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    BackTab,
    /// Ctrl-C: quit from anywhere, the search line included
    Interrupt,
}

impl Key {
    /// hjkl as arrows (outside the search line)
    pub fn navigation(self) -> Key {
        match self {
            Key::Char('h') => Key::Left,
            Key::Char('j') => Key::Down,
            Key::Char('k') => Key::Up,
            Key::Char('l') => Key::Right,
            Key::Char('g') => Key::Home,
            Key::Char('G') => Key::End,
            other => other,
        }
    }
}

/// A key script: keys separated by spaces. Named keys: enter, esc, backspace, up, down, left, right, pgup, pgdn,
/// home, end, tab, backtab, space; any single character is itself; `/text` searches for text (the `/` key, the
/// characters, enter).
pub fn parse_script(script: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for token in script.split_whitespace() {
        let bare = token.trim_start_matches('<').trim_end_matches('>');
        let named = match bare.to_ascii_lowercase().as_str() {
            "enter" | "return" | "cr" => Some(Key::Enter),
            "esc" | "escape" => Some(Key::Esc),
            "backspace" | "bs" => Some(Key::Backspace),
            "up" => Some(Key::Up),
            "down" => Some(Key::Down),
            "left" => Some(Key::Left),
            "right" => Some(Key::Right),
            "pgup" | "pageup" => Some(Key::PageUp),
            "pgdn" | "pagedown" => Some(Key::PageDown),
            "home" => Some(Key::Home),
            "end" => Some(Key::End),
            "tab" => Some(Key::Tab),
            "backtab" | "shift-tab" | "s-tab" => Some(Key::BackTab),
            "space" => Some(Key::Char(' ')),
            _ => None,
        };
        if let Some(key) = named.filter(|_| bare.chars().count() > 1) {
            keys.push(key);
            continue;
        }
        let mut chars = token.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => keys.push(Key::Char(c)),
            (Some('/'), Some(_)) => {
                keys.push(Key::Char('/'));
                keys.extend(token.chars().skip(1).map(Key::Char));
                keys.push(Key::Enter);
            }
            _ => return Err(format!("--keys: no key \"{token}\" (keys: a character, enter, esc, backspace, up, down, left, right, pgup, pgdn, home, end, tab, backtab, space, or /text to search)")),
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_script_reads_names_characters_and_searches() {
        assert_eq!(parse_script("2 j enter <esc> /rank m").unwrap(), vec![Key::Char('2'), Key::Char('j'), Key::Enter, Key::Esc, Key::Char('/'), Key::Char('r'), Key::Char('a'), Key::Char('n'), Key::Char('k'), Key::Enter, Key::Char('m')]);
        assert_eq!(parse_script("pgdn S-TAB space").unwrap(), vec![Key::PageDown, Key::BackTab, Key::Char(' ')]);
        assert!(parse_script("jump").is_err());
    }
}
