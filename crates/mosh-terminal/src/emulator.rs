//! The emulator: parser, dispatcher and framebuffer wired together.
//!
//! Transliterated from `terminal.{h,cc}` and `terminaluserinput.cc`. The C++ dispatches
//! each action through a virtual `act_on_terminal` on the action object; here the
//! emulator matches on the action kind, which is the same dispatch without the friend
//! declarations.

use crate::dispatcher::{Dispatcher, FunctionType};
use crate::framebuffer::{Cell, Framebuffer};
use crate::parser::{Action, Kind, Utf8Parser};

/// Translates the user's keystrokes on their way to the host.
///
/// The user is always in application mode; when the host is not, cursor keys have to be
/// rewritten from SS3 form to ANSI form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserInput {
    state: UserInputState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum UserInputState {
    #[default]
    Ground,
    Esc,
    /// Single shift 3, which introduces a cursor key.
    Ss3,
}

impl UserInput {
    pub fn new() -> Self {
        UserInput::default()
    }

    pub fn input(&mut self, c: u8, application_mode_cursor_keys: bool) -> Vec<u8> {
        // The SS3 state has to look one byte ahead to see whether a cursor key follows.
        //
        // This does not handle the 8-bit SS3 C1 control, which would be two octets in
        // UTF-8. Nothing appears to send it.
        match self.state {
            UserInputState::Ground => {
                if c == 0x1b {
                    self.state = UserInputState::Esc;
                }
                vec![c]
            }
            UserInputState::Esc => {
                if c == b'O' {
                    // ESC O is the 7-bit SS3; hold it back until we see what follows.
                    self.state = UserInputState::Ss3;
                    return Vec::new();
                }
                self.state = UserInputState::Ground;
                vec![c]
            }
            UserInputState::Ss3 => {
                self.state = UserInputState::Ground;
                if !application_mode_cursor_keys && (b'A'..=b'D').contains(&c) {
                    vec![b'[', c]
                } else {
                    vec![b'O', c]
                }
            }
        }
    }
}

/// A terminal: bytes in, screen state out.
#[derive(Debug, Clone)]
pub struct Emulator {
    fb: Framebuffer,
    dispatch: Dispatcher,
    parser: Utf8Parser,
    user: UserInput,
}

impl Emulator {
    pub fn new(width: i32, height: i32) -> Self {
        Emulator {
            fb: Framebuffer::new(width, height),
            dispatch: Dispatcher::new(),
            parser: Utf8Parser::new(),
            user: UserInput::new(),
        }
    }

    pub fn fb(&self) -> &Framebuffer {
        &self.fb
    }

    pub fn fb_mut(&mut self) -> &mut Framebuffer {
        &mut self.fb
    }

    pub fn user_mut(&mut self) -> &mut UserInput {
        &mut self.user
    }

    /// Take the reply the terminal owes the application, clearing it.
    pub fn read_octets_to_host(&mut self) -> String {
        std::mem::take(&mut self.dispatch.terminal_to_host)
    }

    pub fn clear_color_queries(&mut self) {
        self.fb.clear_color_queries();
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        self.fb.resize(width, height);
    }

    /// Feed bytes from the host.
    pub fn input(&mut self, bytes: &[u8]) {
        let mut actions = Vec::new();
        for &b in bytes {
            actions.clear();
            self.parser.input(b, &mut actions);
            for action in std::mem::take(&mut actions) {
                self.act(&action);
                actions.clear();
            }
        }
    }

    /// Apply one parser action.
    pub fn act(&mut self, action: &Action) {
        match action.kind {
            Kind::Print => {
                if let Some(ch) = action.ch {
                    self.print(ch);
                }
            }
            Kind::Execute => {
                if let Some(ch) = action.ch {
                    self.dispatch
                        .dispatch(FunctionType::Control, ch, &mut self.fb);
                }
            }
            Kind::Clear => self.dispatch.clear(),
            Kind::Param => {
                if let Some(ch) = action.ch {
                    self.dispatch.new_param_char(ch);
                }
            }
            Kind::Collect => {
                if let Some(ch) = action.ch {
                    self.dispatch.collect(ch);
                }
            }
            Kind::CsiDispatch => {
                if let Some(ch) = action.ch {
                    self.dispatch.dispatch(FunctionType::Csi, ch, &mut self.fb);
                }
            }
            Kind::EscDispatch => {
                if let Some(ch) = action.ch {
                    self.esc_dispatch(ch);
                }
            }
            Kind::OscStart => self.dispatch.osc_start(),
            Kind::OscPut => {
                if let Some(ch) = action.ch {
                    self.dispatch.osc_put(ch);
                }
            }
            Kind::OscEnd => self.dispatch.osc_dispatch(&mut self.fb),
            // Hook, Put and Unhook carry device control strings, which mosh ignores.
            Kind::Hook | Kind::Put | Kind::Unhook | Kind::Ignore => {}
        }
    }

    fn esc_dispatch(&mut self, ch: char) {
        // Handle the 7-bit ESC encoding of a C1 control character.
        if self.dispatch.dispatch_chars().is_empty() && ('\u{40}'..='\u{5f}').contains(&ch) {
            let c1 = char::from_u32(ch as u32 + 0x40).unwrap_or(ch);
            self.dispatch
                .dispatch(FunctionType::Control, c1, &mut self.fb);
        } else {
            self.dispatch
                .dispatch(FunctionType::Escape, ch, &mut self.fb);
        }
    }

    fn print(&mut self, ch: char) {
        // Check for printing ISO 8859-1 first; a cheap way to catch common narrow text
        // without consulting the locale.
        let chwidth: i32 = if ch == '\0' {
            -1
        } else if Cell::isprint_iso8859_1(ch) {
            1
        } else {
            mosh_sys::wcwidth(ch).map(|w| w as i32).unwrap_or(-1)
        };

        match chwidth {
            1 | 2 => {
                // `None` here means the target cell must be looked up again because the
                // cursor moved.
                let mut have_cell = true;

                if self.fb.ds.auto_wrap_mode && self.fb.ds.next_print_will_wrap {
                    self.fb.mutable_row(-1).set_wrap(true);
                    self.fb.ds.move_col(0, false, false);
                    self.fb.move_rows_autoscroll(1);
                    have_cell = false;
                } else if self.fb.ds.auto_wrap_mode
                    && chwidth == 2
                    && self.fb.ds.cursor_col() == self.fb.ds.width() - 1
                {
                    // Wrap a two-cell character when there is no room, even without the
                    // will-wrap flag.
                    self.fb.reset_cell(-1, -1);
                    self.fb.mutable_row(-1).set_wrap(false);
                    // There is no consistent way to make the downstream terminal set the
                    // wrap-around copy-and-paste flag on a row that ends with an empty
                    // cell because a wide character wrapped to the next line.
                    self.fb.ds.move_col(0, false, false);
                    self.fb.move_rows_autoscroll(1);
                    have_cell = false;
                }

                if self.fb.ds.insert_mode {
                    for _ in 0..chwidth {
                        self.fb
                            .insert_cell(self.fb.ds.cursor_row(), self.fb.ds.cursor_col());
                    }
                    have_cell = false;
                }
                let _ = have_cell;

                let (row, col) = (self.fb.ds.cursor_row(), self.fb.ds.cursor_col());
                self.fb.reset_cell(row, col);
                self.fb.mutable_cell(row, col).append(ch);
                self.fb.mutable_cell(row, col).set_wide(chwidth == 2);
                self.fb.apply_renditions_to_cell(row, col);
                self.fb.apply_hyperlink_to_cell(row, col);

                // A wide character covers the following cell, which must be blanked.
                if chwidth == 2 && col + 1 < self.fb.ds.width() {
                    self.fb.reset_cell(row, col + 1);
                }

                self.fb.ds.move_col(chwidth, true, true);
            }
            0 => {
                // A combining character attaches to the cell holding the base character,
                // which a resize may have moved off screen.
                let Some(cell) = self.fb.combining_cell() else {
                    return;
                };
                if cell.is_empty() {
                    // The cell starts with a combining character. It is not necessarily
                    // the target for a new base character -- at the start of a line, or
                    // after the base was cleared by ED or EL.
                    cell.set_fallback(true);
                    if !cell.is_full() {
                        cell.append(ch);
                    }
                    self.fb.ds.move_col(1, true, true);
                    return;
                }
                if !cell.is_full() {
                    cell.append(ch);
                }
            }
            // Unprintable: dropped, as the C++ does.
            _ => {}
        }
    }
}

impl PartialEq for Emulator {
    /// Only the screen matters; the dispatcher and user-input state are transient.
    fn eq(&self, other: &Self) -> bool {
        self.fb == other.fb
    }
}

impl Eq for Emulator {}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(emu: &Emulator) -> Vec<String> {
        let fb = emu.fb();
        (0..fb.ds.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..fb.ds.width() {
                    fb.cell(y, x).unwrap().print_grapheme(&mut line);
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn plain_text_lands_on_the_screen() {
        let mut emu = Emulator::new(20, 3);
        emu.input(b"hello");
        assert_eq!(screen(&emu)[0], "hello");
        assert_eq!(emu.fb().ds.cursor_col(), 5);
    }

    #[test]
    fn text_wraps_at_the_right_margin() {
        let mut emu = Emulator::new(5, 3);
        emu.input(b"abcdefg");
        assert_eq!(screen(&emu)[0], "abcde");
        assert_eq!(screen(&emu)[1], "fg");
        assert!(emu.fb().row(0).unwrap().wrap());
    }

    #[test]
    fn autowrap_off_overwrites_the_last_column() {
        let mut emu = Emulator::new(5, 2);
        emu.input(b"\x1b[?7l"); // disable auto wrap
        emu.input(b"abcdefg");
        assert_eq!(screen(&emu)[0], "abcdg");
        assert_eq!(screen(&emu)[1], "");
    }

    #[test]
    fn newline_scrolls_at_the_bottom() {
        let mut emu = Emulator::new(5, 2);
        emu.input(b"a\r\nb\r\nc");
        assert_eq!(screen(&emu)[0], "b");
        assert_eq!(screen(&emu)[1], "c");
    }

    #[test]
    fn cursor_addressing_is_one_based() {
        let mut emu = Emulator::new(10, 5);
        emu.input(b"\x1b[3;4Hx");
        assert_eq!(screen(&emu)[2], "   x");
    }

    #[test]
    fn erase_display_clears_the_screen() {
        let mut emu = Emulator::new(10, 3);
        emu.input(b"abc\r\ndef");
        emu.input(b"\x1b[2J");
        assert_eq!(screen(&emu), vec!["", "", ""]);
    }

    #[test]
    fn insert_mode_pushes_text_right() {
        let mut emu = Emulator::new(10, 1);
        emu.input(b"world");
        emu.input(b"\x1b[H");   // home
        emu.input(b"\x1b[4h");  // insert mode
        emu.input(b"AB");
        assert_eq!(screen(&emu)[0], "ABworld");
    }

    #[test]
    fn a_wide_character_occupies_two_cells() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        let mut emu = Emulator::new(10, 1);
        emu.input("\u{4e00}".as_bytes());
        assert!(emu.fb().cell(0, 0).unwrap().wide());
        assert_eq!(emu.fb().ds.cursor_col(), 2);
        // The covered cell is blanked rather than left stale.
        assert!(emu.fb().cell(0, 1).unwrap().is_empty());
    }

    #[test]
    fn a_wide_character_wraps_rather_than_splitting() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        let mut emu = Emulator::new(5, 2);
        emu.input(b"abcd");
        emu.input("\u{4e00}".as_bytes());
        // Only four columns were used, so the wide character moves to the next row.
        assert_eq!(emu.fb().ds.cursor_row(), 1);
        assert!(emu.fb().cell(1, 0).unwrap().wide());
    }

    #[test]
    fn combining_characters_join_the_preceding_cell() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        let mut emu = Emulator::new(10, 1);
        emu.input("e\u{301}".as_bytes()); // e + combining acute
        assert_eq!(emu.fb().cell(0, 0).unwrap().contents(), "e\u{301}");
        assert_eq!(emu.fb().ds.cursor_col(), 1);
    }

    #[test]
    fn a_leading_combining_character_gets_a_base() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        let mut emu = Emulator::new(10, 1);
        emu.input("\u{301}".as_bytes()); // combining acute with nothing before it
        let cell = emu.fb().cell(0, 0).unwrap();
        assert!(cell.fallback(), "no fallback base was set");
        let mut out = String::new();
        cell.print_grapheme(&mut out);
        // Rendered against a no-break space so it has something to combine with.
        assert_eq!(out, "\u{a0}\u{301}");
    }

    #[test]
    fn esc_encoded_c1_controls_act_as_controls() {
        let mut emu = Emulator::new(10, 3);
        emu.input(b"abc");
        // ESC E is the 7-bit encoding of NEL, which is a carriage return plus line feed.
        emu.input(b"\x1bEx");
        assert_eq!(screen(&emu)[0], "abc");
        assert_eq!(screen(&emu)[1], "x");
    }

    #[test]
    fn device_status_reports_reach_the_host() {
        let mut emu = Emulator::new(10, 5);
        emu.input(b"\x1b[6n");
        assert_eq!(emu.read_octets_to_host(), "\x1b[1;1R");
        // Reading it clears it.
        assert_eq!(emu.read_octets_to_host(), "");
    }

    #[test]
    fn renditions_apply_to_printed_cells() {
        let mut emu = Emulator::new(10, 1);
        emu.input(b"\x1b[1mx");
        let cell = emu.fb().cell(0, 0).unwrap();
        assert!(cell
            .renditions()
            .attribute(crate::framebuffer::Attribute::Bold));
    }

    #[test]
    fn hyperlinks_apply_to_printed_cells() {
        let mut emu = Emulator::new(10, 1);
        emu.input(b"\x1b]8;;http://x\x07y");
        assert!(!emu.fb().cell(0, 0).unwrap().hyperlink().is_empty());
    }

    #[test]
    fn user_input_passes_ordinary_keys_through() {
        let mut u = UserInput::new();
        assert_eq!(u.input(b'a', false), vec![b'a']);
    }

    #[test]
    fn user_input_rewrites_cursor_keys_outside_application_mode() {
        let mut u = UserInput::new();
        assert_eq!(u.input(0x1b, false), vec![0x1b]);
        // ESC O is held back until the following byte decides the translation.
        assert!(u.input(b'O', false).is_empty());
        assert_eq!(u.input(b'A', false), vec![b'[', b'A']);
    }

    #[test]
    fn user_input_preserves_cursor_keys_in_application_mode() {
        let mut u = UserInput::new();
        u.input(0x1b, true);
        u.input(b'O', true);
        assert_eq!(u.input(b'A', true), vec![b'O', b'A']);
    }

    #[test]
    fn user_input_passes_non_cursor_ss3_keys_through() {
        let mut u = UserInput::new();
        u.input(0x1b, false);
        u.input(b'O', false);
        // A function key, not a cursor key, so it keeps its SS3 form.
        assert_eq!(u.input(b'P', false), vec![b'O', b'P']);
    }

    #[test]
    fn emulators_compare_by_screen_only() {
        let mut a = Emulator::new(10, 2);
        a.input(b"hi");
        let b = a.clone();
        assert_eq!(a, b);

        // A pending reply to the host is not part of the screen.
        a.input(b"\x1b[6n");
        assert_eq!(a, b);

        // Writing to the screen does separate them.
        a.input(b"!");
        assert_ne!(a, b);
    }

    #[test]
    fn independent_emulators_are_unequal_even_with_the_same_screen() {
        // Rows are compared by identity, matching the C++, so two emulators that were
        // never derived from one another are unequal however similar they look. Erring
        // this way costs a redundant diff; the opposite would lose an update.
        let mut a = Emulator::new(10, 2);
        let mut b = Emulator::new(10, 2);
        a.input(b"hi");
        b.input(b"hi");
        assert_ne!(a, b);
    }
}
