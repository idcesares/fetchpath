//! Themes. The views draw with six named colors, one per role (`view::ACCENT`
//! and the rest); a theme maps each role to a color of its own once a frame
//! is drawn, so the drawing code stays the same for every theme.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

/// What a color means on screen; the order of `ROLES` and a theme's colors.
pub const ROLES: [(&str, Color); 6] = [
    ("accent", Color::Cyan),
    ("dim", Color::DarkGray),
    ("good", Color::Green),
    ("bad", Color::Red),
    ("warn", Color::Yellow),
    ("check", Color::Magenta),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    pub summary: &'static str,
    colors: [Color; 6],
}

pub const BUILT_IN: [Theme; 4] = [
    Theme {
        name: "default",
        summary: "the terminal's own colors",
        colors: [
            Color::Cyan,
            Color::DarkGray,
            Color::Green,
            Color::Red,
            Color::Yellow,
            Color::Magenta,
        ],
    },
    Theme {
        name: "high-contrast",
        summary: "bright colors, at least 7:1 on a dark background",
        colors: [
            Color::Rgb(0, 255, 255),
            Color::Rgb(210, 210, 210),
            Color::Rgb(80, 255, 80),
            Color::Rgb(255, 120, 120),
            Color::Rgb(255, 255, 0),
            Color::Rgb(255, 140, 255),
        ],
    },
    Theme {
        name: "light",
        summary: "dark colors for a light background, at least 4.5:1",
        colors: [
            Color::Rgb(0, 95, 135),
            Color::Rgb(90, 90, 90),
            Color::Rgb(0, 110, 0),
            Color::Rgb(180, 0, 0),
            Color::Rgb(140, 85, 0),
            Color::Rgb(130, 0, 130),
        ],
    },
    Theme {
        name: "plain",
        summary: "no colors; bold and reverse still mark choices",
        colors: [Color::Reset; 6],
    },
];

impl Default for Theme {
    fn default() -> Self {
        BUILT_IN[0]
    }
}

impl Theme {
    pub fn named(name: &str) -> Option<Self> {
        BUILT_IN
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .copied()
    }

    /// Replaces one role's color, for `[colors]` in the file.
    pub fn set(&mut self, role: &str, color: Color) -> bool {
        match ROLES.iter().position(|(name, _)| *name == role) {
            Some(index) => {
                self.colors[index] = color;
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub fn color(&self, role: &str) -> Option<Color> {
        ROLES
            .iter()
            .position(|(name, _)| *name == role)
            .map(|index| self.colors[index])
    }

    fn map(&self, color: Color) -> Color {
        ROLES
            .iter()
            .position(|(_, drawn)| *drawn == color)
            .map_or(color, |index| self.colors[index])
    }

    /// Recolors a freshly drawn buffer. Every buffer is drawn from scratch
    /// before this runs, so each cell is mapped exactly once.
    pub fn apply(&self, buffer: &mut Buffer) {
        if self
            .colors
            .iter()
            .zip(ROLES)
            .all(|(mine, (_, drawn))| *mine == drawn)
        {
            return;
        }
        for cell in &mut buffer.content {
            cell.fg = self.map(cell.fg);
            cell.bg = self.map(cell.bg);
        }
    }
}

/// A color as the file writes it: `default`, a terminal color name such as
/// `cyan` or `light-red`, or `#rrggbb`.
pub fn parse_color(text: &str) -> Result<Color, String> {
    let lower = text.trim().to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix('#') {
        if hex.len() == 6
            && let Ok(value) = u32::from_str_radix(hex, 16)
        {
            return Ok(Color::Rgb(
                (value >> 16) as u8,
                (value >> 8) as u8,
                value as u8,
            ));
        }
        return Err(format!("{text} is not a #rrggbb color"));
    }
    let name: String = lower.chars().filter(|c| *c != '-' && *c != '_').collect();
    Ok(match name.as_str() {
        "default" | "reset" | "none" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => {
            return Err(format!(
                "{text} is not a color; use a name such as cyan or light-red, #rrggbb, or default"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    /// WCAG 2 relative luminance of an sRGB color.
    fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
        let linear = |channel: u8| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("{other:?} is not an RGB color"),
        }
    }

    /// The themes that promise a contrast keep it for every role: against
    /// Windows Terminal's default dark background (Campbell, #0c0c0c) and
    /// against black, or against white for the light theme. The default and
    /// plain themes use the terminal's own palette, so promise nothing.
    #[test]
    fn built_in_themes_keep_their_contrast() {
        let dark = Theme::named("high-contrast").unwrap();
        let light = Theme::named("light").unwrap();
        for (role, _) in ROLES {
            let color = rgb(dark.color(role).unwrap());
            for background in [(12, 12, 12), (0, 0, 0)] {
                let ratio = contrast(color, background);
                assert!(ratio >= 7.0, "high-contrast {role}: {ratio:.2}");
            }
            let ratio = contrast(rgb(light.color(role).unwrap()), (255, 255, 255));
            assert!(ratio >= 4.5, "light {role}: {ratio:.2}");
        }
    }

    #[test]
    fn a_theme_recolors_only_its_roles() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer.set_string(0, 0, "a", Style::new().fg(Color::Cyan));
        buffer.set_string(1, 0, "b", Style::new().fg(Color::DarkGray).bg(Color::Red));
        buffer.set_string(2, 0, "c", Style::new().fg(Color::Blue));
        let mut theme = Theme::named("plain").unwrap();
        theme.set("accent", Color::White);
        theme.apply(&mut buffer);
        assert_eq!(buffer[(0, 0)].fg, Color::White);
        assert_eq!(
            (buffer[(1, 0)].fg, buffer[(1, 0)].bg),
            (Color::Reset, Color::Reset)
        );
        assert_eq!(buffer[(2, 0)].fg, Color::Blue);
    }

    #[test]
    fn colors_read_by_name_or_hex() {
        assert_eq!(parse_color("light-red"), Ok(Color::LightRed));
        assert_eq!(parse_color("#0A0b0C"), Ok(Color::Rgb(10, 11, 12)));
        assert_eq!(parse_color("Default"), Ok(Color::Reset));
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("teal").is_err());
    }
}
