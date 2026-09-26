//! A list to choose from with the keyboard or the mouse: arrows, Home/End,
//! Page Up/Down and digits move; Enter or a click chooses; ←/→ adjust a
//! value in place; the wheel scrolls; Esc closes.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    pub label: String,
    /// Shown right of the label: a current value or a state.
    pub value: String,
    /// Shown on the menu's hint line while the item is selected.
    pub hint: String,
    /// A heading or note that cannot be chosen.
    pub inert: bool,
}

impl Item {
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            hint: hint.into(),
            inert: false,
        }
    }
}

/// What an input did to the menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pick {
    None,
    /// Enter, Space or a click on the item.
    Choose(usize),
    /// ← (-1) or → (+1) on the item.
    Adjust(usize, i64),
    Close,
}

pub struct Menu {
    pub title: String,
    pub items: Vec<Item>,
    pub selected: usize,
    /// The first item shown when the list is longer than its window.
    pub offset: usize,
    /// Keys shown on the frame's bottom edge.
    pub keys: String,
    /// A short message after a change (saved, refused), shown on the hint.
    pub note: Option<String>,
}

/// Most items shown at once.
pub const ROWS: usize = 10;

impl Menu {
    pub fn new(title: impl Into<String>, items: Vec<Item>, keys: impl Into<String>) -> Self {
        let mut menu = Self {
            title: title.into(),
            items,
            selected: 0,
            offset: 0,
            keys: keys.into(),
            note: None,
        };
        menu.select(0, 1);
        menu
    }

    /// Selects `index`, or the nearest choosable item in `direction`.
    pub fn select(&mut self, index: usize, direction: i64) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        let mut at = index.min(count - 1) as i64;
        while (0..count as i64).contains(&at) {
            if !self.items[at as usize].inert {
                self.selected = at as usize;
                break;
            }
            at += direction;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + ROWS {
            self.offset = self.selected + 1 - ROWS;
        }
    }

    fn step(&mut self, by: i64) {
        let target = (self.selected as i64 + by).clamp(0, self.items.len() as i64 - 1);
        self.select(target as usize, by.signum());
    }

    /// The rows on screen: `(item index, item)`.
    pub fn visible(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items.iter().enumerate().skip(self.offset).take(ROWS)
    }

    pub fn key(&mut self, key: KeyEvent) -> Pick {
        if key.kind == KeyEventKind::Release {
            return Pick::None;
        }
        self.note = None;
        match key.code {
            KeyCode::Esc => Pick::Close,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Pick::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(-1);
                Pick::None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.step(1);
                Pick::None
            }
            KeyCode::PageUp => {
                self.step(-(ROWS as i64));
                Pick::None
            }
            KeyCode::PageDown => {
                self.step(ROWS as i64);
                Pick::None
            }
            KeyCode::Home => {
                self.select(0, 1);
                Pick::None
            }
            KeyCode::End => {
                self.select(usize::MAX, -1);
                Pick::None
            }
            KeyCode::Left => Pick::Adjust(self.selected, -1),
            KeyCode::Right => Pick::Adjust(self.selected, 1),
            KeyCode::Enter | KeyCode::Char(' ') => Pick::Choose(self.selected),
            KeyCode::Char(digit @ '1'..='9') => {
                let index = self.offset + (digit as usize - '1' as usize);
                if self.items.get(index).is_some_and(|item| !item.inert) {
                    self.selected = index;
                    Pick::Choose(index)
                } else {
                    Pick::None
                }
            }
            _ => Pick::None,
        }
    }

    /// A mouse event over the menu. `row_of` says which item, if any, is
    /// drawn on a screen row.
    pub fn mouse(&mut self, event: MouseEvent, row_of: impl Fn(u16) -> Option<usize>) -> Pick {
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.step(-1);
                Pick::None
            }
            MouseEventKind::ScrollDown => {
                self.step(1);
                Pick::None
            }
            MouseEventKind::Down(MouseButton::Left) => match row_of(event.row) {
                Some(index) if !self.items[index].inert => {
                    self.note = None;
                    self.selected = index;
                    Pick::Choose(index)
                }
                _ => Pick::None,
            },
            MouseEventKind::Down(MouseButton::Right) => Pick::Close,
            _ => Pick::None,
        }
    }

    /// The hint for the selected item, or the last note.
    pub fn hint(&self) -> &str {
        self.note
            .as_deref()
            .or_else(|| self.items.get(self.selected).map(|item| item.hint.as_str()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(count: usize) -> Menu {
        let mut items: Vec<Item> = (0..count)
            .map(|n| Item::new(format!("item {n}"), "", ""))
            .collect();
        items[0].inert = true;
        Menu::new("Test", items, "")
    }

    fn press(menu: &mut Menu, code: KeyCode) -> Pick {
        menu.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn keys_skip_headings_and_scroll_the_window() {
        let mut menu = menu(25);
        assert_eq!(menu.selected, 1, "the heading cannot be selected");
        press(&mut menu, KeyCode::Up);
        assert_eq!(menu.selected, 1);
        press(&mut menu, KeyCode::PageDown);
        assert_eq!(menu.selected, 11);
        assert_eq!(menu.offset, 2);
        press(&mut menu, KeyCode::End);
        assert_eq!((menu.selected, menu.offset), (24, 15));
        assert_eq!(press(&mut menu, KeyCode::Enter), Pick::Choose(24));
        assert_eq!(press(&mut menu, KeyCode::Left), Pick::Adjust(24, -1));
        assert_eq!(press(&mut menu, KeyCode::Char('2')), Pick::Choose(16));
        assert_eq!(press(&mut menu, KeyCode::Esc), Pick::Close);
    }

    #[test]
    fn a_click_chooses_the_item_under_it_and_the_wheel_moves() {
        let mut menu = menu(5);
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Rows 10.. show items 0..
        let row_of = |row: u16| row.checked_sub(10).map(usize::from).filter(|i| *i < 5);
        assert_eq!(menu.mouse(click(13), row_of), Pick::Choose(3));
        assert_eq!(
            menu.mouse(click(10), row_of),
            Pick::None,
            "headings ignore clicks"
        );
        assert_eq!(menu.mouse(click(2), row_of), Pick::None);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..click(0)
        };
        menu.mouse(wheel, row_of);
        assert_eq!(menu.selected, 4);
    }
}
