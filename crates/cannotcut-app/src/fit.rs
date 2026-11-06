//! Rows of controls that fit the width they are given, from a wide screen down to the smallest
//! window. As the room shrinks, controls first take their compact forms, then the least needed
//! move into a menu, opened by a button the row gains for it. Widths are reckoned from the
//! words, not measured, so the reckoning errs on the wide side.

use iced::widget::{Row, row};
use iced::{Center, Element};

/// A button's padding on its two sides together.
pub const BUTTON_PADDING: f32 = 20.0;

/// What words in the UI's 16-pixel text take, in logical pixels: per letter, as measured on
/// Windows' Segoe UI, a little over.
pub fn text_width(words: &str) -> f32 {
    words.chars().map(letter_width).sum()
}

/// A button with these words on it.
pub fn button_width(words: &str) -> f32 {
    text_width(words) + BUTTON_PADDING
}

fn letter_width(letter: char) -> f32 {
    match letter {
        'i' | 'l' | 'j' | '\'' | '.' | ',' | ':' | ';' | '|' | '!' => 4.0,
        'I' => 4.5,
        ' ' => 4.5,
        't' | 'f' | 'r' => 5.5,
        '/' | '(' | ')' | '[' | ']' => 6.0,
        'c' | 's' | 'z' | 'x' | 'y' | 'v' | 'k' => 7.5,
        '0'..='9' => 8.5,
        'm' | 'w' => 14.0,
        'M' | 'W' => 14.0,
        'C' | 'O' | 'D' | 'N' | 'U' | 'H' | 'G' | 'Q' => 11.0,
        letter if letter.is_ascii_lowercase() => 9.0,
        letter if letter.is_ascii_uppercase() => 10.5,
        _ => 13.0,
    }
}

/// A control in a row, in its full form.
pub struct Control<'a, Message> {
    full: (Element<'a, Message>, f32),
    compact: Option<(Element<'a, Message>, f32)>,
    /// Which stay longest: the higher, the later it moves to the menu.
    keep: u8,
    /// Its entry in the menu, once there is no room for it: words, and what it does (None while
    /// it can do nothing). Without one it stays in the row.
    entry: Option<(String, Option<Message>)>,
}

impl<'a, Message> Control<'a, Message> {
    /// A control reckoned to take `width`; it stays in the row.
    pub fn new(element: impl Into<Element<'a, Message>>, width: f32) -> Self {
        Self {
            full: (element.into(), width),
            compact: None,
            keep: u8::MAX,
            entry: None,
        }
    }

    /// The form it takes when room is short.
    pub fn compact(mut self, element: impl Into<Element<'a, Message>>, width: f32) -> Self {
        self.compact = Some((element.into(), width));
        self
    }

    /// When even compact controls do not fit, it moves to the menu as `words` (the lowest
    /// `keep` first).
    pub fn or_in_menu(mut self, keep: u8, words: impl Into<String>, does: Option<Message>) -> Self {
        self.keep = keep;
        self.entry = Some((words.into(), does));
        self
    }
}

/// A row as it fits, and what went to its menu.
pub struct Fitted<'a, Message> {
    pub row: Row<'a, Message>,
    /// The menu's entries, in the row's order: words, and what each does.
    pub menu: Vec<(String, Option<Message>)>,
    /// How far into the row the menu's button is reckoned to start.
    pub menu_x: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Form {
    Full,
    Compact,
    InMenu,
}

/// Lays `controls` out in `room` logical pixels with `spacing` between them. When some go to
/// the menu, `menu_button` (reckoned `menu_width` wide) goes in after the first `menu_after`
/// controls.
pub fn fit<'a, Message>(
    controls: Vec<Control<'a, Message>>,
    room: f32,
    spacing: f32,
    menu_button: Element<'a, Message>,
    menu_width: f32,
    menu_after: usize,
) -> Fitted<'a, Message> {
    let mut forms: Vec<Form> = vec![Form::Full; controls.len()];
    choose(&controls, &mut forms, room, spacing, menu_width);
    let mut built = row![].spacing(spacing).align_y(Center);
    let mut menu = Vec::new();
    let mut menu_button = forms.contains(&Form::InMenu).then_some(menu_button);
    let mut menu_x = 0.0;
    let mut x = 0.0;
    for (index, (control, form)) in controls.into_iter().zip(forms).enumerate() {
        if index == menu_after
            && let Some(button) = menu_button.take()
        {
            menu_x = x;
            x += menu_width + spacing;
            built = built.push(button);
        }
        match form {
            Form::Full => {
                x += control.full.1 + spacing;
                built = built.push(control.full.0);
            }
            Form::Compact => {
                let (element, width) = control.compact.expect("only controls with one");
                x += width + spacing;
                built = built.push(element);
            }
            Form::InMenu => menu.extend(control.entry),
        }
    }
    if let Some(button) = menu_button {
        menu_x = x;
        built = built.push(button);
    }
    Fitted {
        row: built,
        menu,
        menu_x,
    }
}

/// Picks each control's form: full if all fit, else compact, else the least kept move to the
/// menu one by one until the rest fit or none can move. Whether they fit in the end.
fn choose<Message>(
    controls: &[Control<'_, Message>],
    forms: &mut [Form],
    room: f32,
    spacing: f32,
    menu_width: f32,
) -> bool {
    let width = |forms: &[Form]| {
        let mut total = 0.0;
        let mut count = 0;
        for (control, form) in controls.iter().zip(forms) {
            let width = match form {
                Form::Full => control.full.1,
                Form::Compact => control.compact.as_ref().map_or(control.full.1, |c| c.1),
                Form::InMenu => continue,
            };
            total += width;
            count += 1;
        }
        if forms.contains(&Form::InMenu) {
            total += menu_width;
            count += 1;
        }
        total + spacing * (count.max(1) - 1) as f32
    };
    if width(forms) <= room {
        return true;
    }
    for (control, form) in controls.iter().zip(forms.iter_mut()) {
        if control.compact.is_some() {
            *form = Form::Compact;
        }
    }
    while width(forms) > room {
        // The least kept goes first; of two kept alike, the one further along the row.
        let next = controls
            .iter()
            .zip(forms.iter())
            .enumerate()
            .filter(|(_, (control, form))| control.entry.is_some() && **form != Form::InMenu)
            .min_by_key(|(index, (control, _))| (control.keep, usize::MAX - index))
            .map(|(index, _)| index);
        match next {
            Some(index) => forms[index] = Form::InMenu,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::space;

    /// Three controls: "a" (100, or 60 compact, kept 5), "b" (100, kept 1) and "c" (100,
    /// stays), 10 apart, the menu button (30) after the first two.
    fn three(room: f32) -> (Vec<(String, Option<u8>)>, f32) {
        let controls: Vec<Control<'_, u8>> = vec![
            Control::new(space(), 100.0)
                .compact(space(), 60.0)
                .or_in_menu(5, "a", Some(1)),
            Control::new(space(), 100.0).or_in_menu(1, "b", None),
            Control::new(space(), 100.0),
        ];
        let fitted = fit(controls, room, 10.0, space().into(), 30.0, 2);
        (fitted.menu, fitted.menu_x)
    }

    #[test]
    fn controls_go_compact_then_the_least_kept_go_to_the_menu() {
        // All at full size: 320.
        assert_eq!(three(320.0).0, []);
        // "a" compact: 280.
        assert_eq!(three(300.0).0, []);
        // "b" to the menu, whose button takes its place: 60 + 30 + 100 and two gaps.
        let (menu, x) = three(279.0);
        assert_eq!(menu, [("b".to_string(), None)]);
        assert_eq!(x, 70.0);
        // Then "a": the menu button first in the row.
        let (menu, x) = three(200.0);
        assert_eq!(menu, [("a".to_string(), Some(1)), ("b".to_string(), None)]);
        assert_eq!(x, 0.0);
        // "c" stays, however little room there is.
        assert_eq!(three(10.0).0.len(), 2);
    }

    #[test]
    fn words_are_reckoned_a_little_wide() {
        // Measured on the GPD at 225%: buttons of these words, in logical pixels.
        for (words, measured) in [
            ("Add clips", 84.9),
            ("New project", 105.8),
            ("Open", 58.7),
            ("Save", 52.9),
            ("Export", 64.9),
            ("Select", 62.2),
            ("Step inside", 97.3),
            ("Compound", 99.6),
            ("Undo", 58.7),
            ("Redo", 56.9),
            ("Split", 51.1),
            ("Delete", 65.8),
            ("Add track", 88.0),
        ] {
            let reckoned = button_width(words);
            assert!(
                reckoned >= measured - 1.0 && reckoned <= measured * 1.1,
                "{words}: {reckoned} for {measured}"
            );
        }
    }
}
