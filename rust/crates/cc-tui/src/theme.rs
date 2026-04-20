//! Centralised colour palette for the TUI.
//!
//! Phase D1 ports `utils/theme.ts` into a single Rust module so every
//! Ratatui span can fetch its colour from one place. Two render modes are
//! supported today:
//!
//!   * **TrueColor** — modern terminals advertising `COLORTERM=truecolor`
//!     (or `24bit`); colours are emitted as `Color::Rgb(...)`.
//!   * **Indexed256** — fallback for `TERM=*-256color`; RGB triples are
//!     mapped to the closest xterm-256 palette index via `rgb_to_256`.
//!
//! A deliberately minimal third mode (Ansi16) keeps the worst-case path
//! readable on `TERM=dumb` or pre-ECMA-48 terminals; the mapping is a flat
//! "round to the nearest of red/green/blue/yellow/magenta/cyan/white".
//!
//! There is intentionally no theme-switching UX in D1 — light + ansi-only
//! variants are deferred to a D6 polish pass per plan §2.

use ratatui::style::Color;

/// Render mode discovered from the terminal environment. Tests can construct
/// a [`Theme`] for a specific mode via [`Theme::with_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit RGB. The default for terminals that advertise `COLORTERM`.
    TrueColor,
    /// 256-colour indexed palette. Used when only `TERM=*-256color` is set.
    Indexed256,
    /// 16-colour ANSI fallback. Used for `TERM=dumb` and as a last resort.
    Ansi16,
}

/// Resolved palette. All colours are pre-mapped for the active [`ColorMode`]
/// so call sites can `theme.claude_orange` without re-computing on every draw.
#[derive(Debug, Clone)]
pub struct Theme {
    pub mode: ColorMode,
    /// Claude brand orange (rgb 215,119,87) — user gutter, accents.
    pub claude_orange: Color,
    /// Bash tool border (rgb 255,0,135) — Bash header + the `!` mode prefix.
    pub bash_pink: Color,
    /// Permission prompt accent (rgb 87,105,247) — modal border + permission
    /// help footer.
    pub permission_blue: Color,
    /// Success tick (rgb 44,122,57).
    pub success: Color,
    /// Error tick (rgb 171,43,63).
    pub error: Color,
    /// Warning / system notice (rgb 255,193,7).
    pub warning: Color,
    /// Dim grey for footer hints, placeholder text (rgb 175,175,175).
    pub dim: Color,
    /// Default white text body (rgb 255,255,255).
    pub text: Color,
    /// Inactive grey for separators (rgb 102,102,102).
    pub subtle: Color,
    /// Read / Glob / Grep tool accent (rgb 122,180,232) — soft blue.
    pub info: Color,
    /// Web tools accent (rgb 175,135,255) — electric violet.
    pub web: Color,
}

impl Theme {
    /// Build a theme for the given [`ColorMode`]. Stable across calls (no
    /// allocations beyond the struct itself) so it is cheap to invoke per draw.
    pub fn with_mode(mode: ColorMode) -> Self {
        let pick = |r: u8, g: u8, b: u8| -> Color {
            match mode {
                ColorMode::TrueColor => Color::Rgb(r, g, b),
                ColorMode::Indexed256 => Color::Indexed(rgb_to_256(r, g, b)),
                ColorMode::Ansi16 => rgb_to_ansi16(r, g, b),
            }
        };
        Self {
            mode,
            claude_orange: pick(215, 119, 87),
            bash_pink: pick(255, 0, 135),
            permission_blue: pick(87, 105, 247),
            success: pick(44, 122, 57),
            error: pick(171, 43, 63),
            warning: pick(255, 193, 7),
            dim: pick(175, 175, 175),
            text: pick(255, 255, 255),
            subtle: pick(102, 102, 102),
            info: pick(122, 180, 232),
            web: pick(175, 135, 255),
        }
    }

    /// Detect the active terminal capability and build a theme.
    pub fn detect() -> Self {
        Self::with_mode(detect_mode())
    }

    /// Tool-name → colour mapping. Centralised here so render.rs stays free
    /// of literal `Color::*` values. Falls back to [`Theme::web`] for unknown
    /// tools so MCP-server-prefixed names (`server::tool`) still stand out.
    pub fn tool_color(&self, name: &str) -> Color {
        match name {
            "Bash" => self.bash_pink,
            "Edit" | "Write" | "MultiEdit" => self.claude_orange,
            "Read" => self.info,
            "Grep" | "Glob" => self.info,
            "WebFetch" | "WebSearch" => self.web,
            n if n.contains("::") => self.subtle, // MCP server::tool
            _ => self.web,
        }
    }
}

/// Public accessor — single source of truth for every render path. The
/// terminal capability is sampled per call so `COLORTERM` overrides set by a
/// debugger or test wrapper take effect immediately.
pub fn current() -> Theme {
    Theme::detect()
}

fn detect_mode() -> ColorMode {
    if let Ok(v) = std::env::var("COLORTERM") {
        let v = v.to_ascii_lowercase();
        if v == "truecolor" || v == "24bit" {
            return ColorMode::TrueColor;
        }
    }
    if let Ok(t) = std::env::var("TERM") {
        if t == "dumb" {
            return ColorMode::Ansi16;
        }
        if t.contains("256color")
            || t.contains("kitty")
            || t.contains("alacritty")
            || t.contains("ghostty")
        {
            return ColorMode::Indexed256;
        }
    }
    // Modern terminals (iTerm2, Terminal.app on recent macOS, WezTerm) almost
    // always support truecolor even when they forget to set COLORTERM, so the
    // default is permissive. A user on a strictly 16-colour terminal can
    // export `TERM=dumb` to force the Ansi16 fallback.
    ColorMode::TrueColor
}

/// Map an RGB triple to the closest xterm-256 palette index.
///
/// The xterm-256 layout is:
///   * 0-15  — system colours (matches ANSI 16)
///   * 16-231 — 6×6×6 RGB cube
///   * 232-255 — 24-step grayscale ramp
///
/// We pick from the cube unless the colour is very close to grey, in which
/// case the ramp gives a noticeably better match. This is the standard
/// algorithm used by `tput`, `chalk`, and (importantly) it sends bash-pink
/// `rgb(255,0,135)` to index 198 — the value AC-V8 asserts.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Grayscale ramp first when the channels are close enough that the cube
    // would just produce a desaturated approximation.
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 8 {
        let avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        if avg < 8 {
            return 16;
        }
        if avg > 248 {
            return 231;
        }
        return ((avg as u16 - 8) * 24 / 240) as u8 + 232;
    }
    // 6×6×6 colour cube. Index breakpoints (0,95,135,175,215,255) match
    // xterm; using `(v.saturating_sub(35)) / 40` clipped to [0,5] is the
    // canonical mapping.
    let to6 = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v - 35) / 40).min(5)
        }
    };
    16 + 36 * to6(r) + 6 * to6(g) + to6(b)
}

/// Coarse RGB → ANSI-16 mapping for `TERM=dumb`. Picks the dominant primary
/// channel and returns a saturated ANSI colour. Greys collapse to white/dim.
fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 32 {
        return if max < 64 {
            Color::Black
        } else if max < 192 {
            Color::Gray
        } else {
            Color::White
        };
    }
    let bright = max > 192;
    match (r >= 128, g >= 128, b >= 128) {
        (true, true, true) => Color::White,
        (true, true, false) => {
            if bright {
                Color::LightYellow
            } else {
                Color::Yellow
            }
        }
        (true, false, true) => {
            if bright {
                Color::LightMagenta
            } else {
                Color::Magenta
            }
        }
        (false, true, true) => {
            if bright {
                Color::LightCyan
            } else {
                Color::Cyan
            }
        }
        (true, false, false) => {
            if bright {
                Color::LightRed
            } else {
                Color::Red
            }
        }
        (false, true, false) => {
            if bright {
                Color::LightGreen
            } else {
                Color::Green
            }
        }
        (false, false, true) => {
            if bright {
                Color::LightBlue
            } else {
                Color::Blue
            }
        }
        (false, false, false) => Color::Black,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-V8 — Bash header colour on a truecolor terminal must be the exact
    /// rgb triple (255,0,135). The renderer reads the colour from
    /// [`Theme::tool_color`], so anchoring the test there exercises both the
    /// palette mapping and the tool-name lookup.
    #[test]
    fn bash_tool_color_is_bash_pink_on_truecolor() {
        let theme = Theme::with_mode(ColorMode::TrueColor);
        assert_eq!(theme.tool_color("Bash"), Color::Rgb(255, 0, 135));
    }

    /// AC-V8 — same colour, downgraded to xterm-256 index 198 (DeepPink2).
    /// The mapping algorithm is exercised end-to-end so a regression in
    /// [`rgb_to_256`] cannot silently shift the palette.
    #[test]
    fn bash_tool_color_is_indexed_198_on_xterm_256color() {
        let theme = Theme::with_mode(ColorMode::Indexed256);
        assert_eq!(theme.tool_color("Bash"), Color::Indexed(198));
    }

    /// Every well-known truecolor → 256-index mapping that downstream tests
    /// might rely on. If any of these break, every snapshot under the 256
    /// path needs re-checking.
    #[test]
    fn rgb_to_256_known_anchors() {
        // Bash pink — anchor for AC-V8.
        assert_eq!(rgb_to_256(255, 0, 135), 198);
        // Pure white / black collapse to grayscale endpoints.
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        // Mid-grey lands somewhere in the 232..255 ramp.
        let g = rgb_to_256(128, 128, 128);
        assert!((232..=255).contains(&g), "mid-grey index {g} out of ramp");
    }

    /// Sanity check that the ANSI-16 fallback does not panic for the colours
    /// the renderer touches and that bash-pink at least surfaces a magenta
    /// shade rather than collapsing to grey.
    #[test]
    fn ansi16_fallback_picks_a_visible_colour_for_bash_pink() {
        let theme = Theme::with_mode(ColorMode::Ansi16);
        let c = theme.tool_color("Bash");
        assert!(
            matches!(c, Color::Magenta | Color::LightMagenta),
            "got {c:?}"
        );
    }

    /// `tool_color` falls back to the web-tools accent for unknown tools so
    /// new MCP servers stay visible without a render.rs change.
    #[test]
    fn unknown_tool_falls_back_to_web_accent() {
        let theme = Theme::with_mode(ColorMode::TrueColor);
        assert_eq!(theme.tool_color("WhateverTool"), theme.web);
    }
}
