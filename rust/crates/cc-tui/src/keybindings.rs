//! User-customizable keybindings loaded from `~/.claude/keybindings.json`.
//!
//! Schema (all fields optional, fall back to defaults):
//!
//! ```json
//! {
//!   "quit":   "ctrl+q",
//!   "abort":  "ctrl+c",
//!   "submit": "enter"
//! }
//! ```
//!
//! Recognized chord syntax: `ctrl+x`, `alt+x`, `shift+x`, `enter`, `esc`,
//! `tab`, `backspace`, `up`, `down`, `left`, `right`, `pageup`, `pagedown`,
//! `home`, `end`, `f1`..`f12`, or any single character.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Abort,
    Submit,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Raw {
    #[serde(default)]
    quit: Option<String>,
    #[serde(default)]
    abort: Option<String>,
    #[serde(default)]
    submit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Keybindings {
    pub quit: Chord,
    pub abort: Chord,
    pub submit: Chord,
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings {
            quit: Chord::ctrl('q'),
            abort: Chord::ctrl('c'),
            submit: Chord::enter(),
        }
    }
}

impl Keybindings {
    /// Load `~/.claude/keybindings.json`, falling back to defaults.
    /// Bad files are logged and ignored.
    pub fn load() -> Self {
        let Some(path) = path() else { return Self::default() };
        Self::load_from(&path)
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let mut kb = Keybindings::default();
        let raw_text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return kb,
        };
        let raw: Raw = match serde_json::from_str(&raw_text) {
            Ok(r) => r,
            Err(e) => {
                debug!("cc-tui: bad keybindings.json: {e}");
                return kb;
            }
        };
        if let Some(s) = raw.quit.as_deref().and_then(parse_chord) {
            kb.quit = s;
        }
        if let Some(s) = raw.abort.as_deref().and_then(parse_chord) {
            kb.abort = s;
        }
        if let Some(s) = raw.submit.as_deref().and_then(parse_chord) {
            kb.submit = s;
        }
        kb
    }

    /// Match a key event against the bound actions.
    pub fn match_action(&self, key: &KeyEvent) -> Option<Action> {
        if self.quit.matches(key) {
            return Some(Action::Quit);
        }
        if self.abort.matches(key) {
            return Some(Action::Abort);
        }
        if self.submit.matches(key) {
            return Some(Action::Submit);
        }
        None
    }
}

fn path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("keybindings.json"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn ctrl(c: char) -> Self {
        Chord {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }
    pub fn enter() -> Self {
        Chord {
            code: KeyCode::Enter,
            mods: KeyModifiers::NONE,
        }
    }
    pub fn matches(&self, key: &KeyEvent) -> bool {
        // Crossterm sometimes sends an empty modifier set with Char keys; only
        // compare the relevant CTRL/ALT/SHIFT bits.
        const RELEVANT: KeyModifiers = KeyModifiers::from_bits_truncate(
            KeyModifiers::CONTROL.bits() | KeyModifiers::ALT.bits() | KeyModifiers::SHIFT.bits(),
        );
        let want = self.mods & RELEVANT;
        let got = key.modifiers & RELEVANT;
        self.code == key.code && want == got
    }
}

pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut mods = KeyModifiers::NONE;
    let mut parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let key = parts.pop()?;
    for p in parts {
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let code = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        other if other.len() == 1 => KeyCode::Char(other.chars().next().unwrap()),
        other if other.starts_with('f') && other.len() <= 3 => {
            let n: u8 = other[1..].parse().ok()?;
            KeyCode::F(n)
        }
        _ => return None,
    };
    Some(Chord { code, mods })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_ctrl_q() {
        let c = parse_chord("ctrl+q").unwrap();
        assert_eq!(c.code, KeyCode::Char('q'));
        assert_eq!(c.mods, KeyModifiers::CONTROL);
    }

    #[test]
    fn parses_enter_alone() {
        let c = parse_chord("enter").unwrap();
        assert_eq!(c.code, KeyCode::Enter);
    }

    #[test]
    fn defaults_when_file_missing() {
        let tmp = tempdir().unwrap();
        let kb = Keybindings::load_from(&tmp.path().join("nope.json"));
        assert_eq!(kb.quit, Chord::ctrl('q'));
    }

    #[test]
    fn loads_overrides_from_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("kb.json");
        fs::write(&path, r#"{"quit":"ctrl+x","submit":"enter"}"#).unwrap();
        let kb = Keybindings::load_from(&path);
        assert_eq!(kb.quit, Chord::ctrl('x'));
        assert_eq!(kb.submit, Chord::enter());
        // unspecified fields keep defaults
        assert_eq!(kb.abort, Chord::ctrl('c'));
    }

    #[test]
    fn match_action_recognises_quit() {
        let kb = Keybindings::default();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(kb.match_action(&key), Some(Action::Quit));
    }

    #[test]
    fn match_action_ignores_unrelated_keys() {
        let kb = Keybindings::default();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(kb.match_action(&key), None);
    }
}
