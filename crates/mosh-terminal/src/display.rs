//! Turning a screen into the bytes that draw it.
//!
//! Transliterated from `terminaldisplay.{h,cc}`.
//!
//! Every byte here reaches the user's real terminal and is compared directly by the
//! end-to-end tests, so the output must match the C++ exactly -- including which
//! shortcut is chosen for a given cursor move.

use std::rc::Rc;

use crate::framebuffer::{
    Cell, ColorType, Framebuffer, Hyperlink, MouseEncodingMode, MouseReportingMode, Renditions, Row,
};

fn initial_rendition() -> Renditions {
    Renditions::new(0)
}

/// Working state while building one frame.
pub struct FrameState {
    pub str: String,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub current_rendition: Renditions,
    pub current_hyperlink: Hyperlink,
    pub cursor_visible: bool,
}

impl FrameState {
    fn new(last: &Framebuffer) -> Self {
        FrameState {
            // A guess at the size; wrong only costs a reallocation.
            str: String::with_capacity((last.ds.width() * last.ds.height() * 4).max(0) as usize),
            cursor_x: 0,
            cursor_y: 0,
            current_rendition: Renditions::new(0),
            current_hyperlink: Hyperlink::default(),
            cursor_visible: last.ds.cursor_visible,
        }
    }

    fn append(&mut self, s: &str) {
        self.str.push_str(s);
    }

    fn append_char(&mut self, c: char) {
        self.str.push(c);
    }

    fn append_repeated(&mut self, n: usize, c: char) {
        for _ in 0..n {
            self.str.push(c);
        }
    }

    fn append_cell(&mut self, cell: &Cell) {
        cell.print_grapheme(&mut self.str);
    }

    /// Move the cursor, hiding it first so the move is not seen.
    fn append_silent_move(&mut self, y: i32, x: i32) {
        if self.cursor_x == x && self.cursor_y == y {
            return;
        }
        if self.cursor_visible {
            self.append("\x1b[?25l");
            self.cursor_visible = false;
        }
        self.append_move(y, x);
    }

    fn append_move(&mut self, y: i32, x: i32) {
        let last_x = self.cursor_x;
        let last_y = self.cursor_y;
        self.cursor_x = x;
        self.cursor_y = y;

        // Only worth optimising when we know where the cursor actually is.
        if last_x != -1 && last_y != -1 {
            // Carriage return and line feeds are cheap and easier to read in a trace.
            if x == 0 && y - last_y >= 0 && y - last_y < 5 {
                if last_x != 0 {
                    self.append_char('\r');
                }
                self.append_repeated((y - last_y) as usize, '\n');
                return;
            }
            // Backspaces are good too.
            if y == last_y && x - last_x < 0 && x - last_x > -5 {
                self.append_repeated((last_x - x) as usize, '\u{8}');
                return;
            }
        }

        self.append(&format!("\x1b[{};{}H", y + 1, x + 1));
    }

    fn update_rendition(&mut self, r: &Renditions, force: bool) {
        if force || self.current_rendition != *r {
            let sgr = r.sgr();
            self.append(&sgr);
            self.current_rendition = *r;
        }
    }

    fn update_hyperlink(&mut self, h: &Hyperlink, force: bool) {
        if force || self.current_hyperlink != *h {
            let osc8 = h.osc8();
            self.append(&osc8);
            self.current_hyperlink = h.clone();
        }
    }
}

/// Knows what the user's terminal can do, and writes frames for it.
pub struct Display {
    /// Erase character. Part of vt200, but tmux does not support it (nor does the
    /// "screen" terminfo entry that tmux advertises).
    has_ech: bool,
    /// Erases fill the cell with the background colour.
    has_bce: bool,
    /// Supports setting the window title and icon name.
    has_title: bool,
    /// Enter and leave the alternate screen.
    smcup: Option<String>,
    rmcup: Option<String>,
}

/// terminfo has no reliable capability for window titles, so the C++ hardcodes a list of
/// terminal types known to support them. Kept verbatim.
const TITLE_TERM_TYPES: &[&str] = &[
    "xterm",
    "rxvt",
    "kterm",
    "Eterm",
    "alacritty",
    "screen",
    "tmux",
];

impl Display {
    /// Query the terminal's capabilities, or assume a capable terminal.
    pub fn new(use_environment: bool) -> Result<Self, mosh_sys::terminfo::TermError> {
        let mut display = Display {
            has_ech: true,
            has_bce: true,
            has_title: true,
            smcup: None,
            rmcup: None,
        };

        if !use_environment {
            return Ok(display);
        }

        mosh_sys::terminfo::setup()?;

        display.has_ech = mosh_sys::terminfo::string("ech")?.is_some();
        display.has_bce = mosh_sys::terminfo::flag("bce")?;

        display.has_title = std::env::var("TERM")
            .map(|term| TITLE_TERM_TYPES.iter().any(|p| term.starts_with(p)))
            .unwrap_or(false);

        if std::env::var_os("MOSH_NO_TERM_INIT").is_none() {
            display.smcup = mosh_sys::terminfo::string("smcup")?.map(bytes_to_string);
            display.rmcup = mosh_sys::terminfo::string("rmcup")?.map(bytes_to_string);
        }

        Ok(display)
    }

    pub fn open(&self) -> String {
        format!("{}\x1b[?1h", self.smcup.as_deref().unwrap_or(""))
    }

    pub fn close(&self) -> String {
        format!(
            "\x1b[?1l\x1b[0m\x1b[?25h\
             \x1b[?1003l\x1b[?1002l\x1b[?1001l\x1b[?1000l\
             \x1b[?1015l\x1b[?1006l\x1b[?1005l{}",
            self.rmcup.as_deref().unwrap_or("")
        )
    }

    fn can_use_erase(&self, frame: &FrameState) -> bool {
        self.has_bce
            || (frame.current_rendition == initial_rendition()
                && frame.current_hyperlink.is_empty())
    }

    /// The smallest byte sequence that turns `last` into `f` on a real terminal.
    pub fn new_frame(&self, mut initialized: bool, last: &Framebuffer, f: &Framebuffer) -> String {
        let mut frame = FrameState::new(last);

        // Has the bell been rung?
        if f.bell_count() != last.bell_count() {
            frame.append_char('\u{7}');
        }

        // Has the icon name or window title changed?
        if self.has_title
            && f.is_title_initialized()
            && (!initialized
                || f.icon_name() != last.icon_name()
                || f.window_title() != last.window_title())
        {
            if f.icon_name() == f.window_title() {
                // One sequence sets both.
                frame.append("\x1b]0;");
                for &c in f.window_title() {
                    frame.append_char(c);
                }
                // ST is more correct, but BEL is more widely supported.
                frame.append_char('\u{7}');
            } else {
                frame.append("\x1b]1;");
                for &c in f.icon_name() {
                    frame.append_char(c);
                }
                frame.append_char('\u{7}');

                frame.append("\x1b]2;");
                for &c in f.window_title() {
                    frame.append_char(c);
                }
                frame.append_char('\u{7}');
            }
        }

        // Has the clipboard been set or queried? Gated on `initialized` so a client
        // attaching or repainting does not replay a stale copy.
        if initialized && f.clipboard_seq() != last.clipboard_seq() {
            frame.append("\x1b]52;");
            for &c in f.clipboard() {
                frame.append_char(c);
            }
            frame.append_char('\u{7}');
        }

        // Has the application asked the terminal to report a colour? Gated for the same
        // reason as the clipboard.
        if initialized && f.color_query_seq() != last.color_query_seq() {
            let queries = f.color_queries().to_string();
            frame.append(&queries);
        }

        // Has reverse video changed?
        if !initialized || f.ds.reverse_video != last.ds.reverse_video {
            frame.append(if f.ds.reverse_video {
                "\x1b[?5h"
            } else {
                "\x1b[?5l"
            });
        }

        // Has the size changed?
        if !initialized || f.ds.width() != last.ds.width() || f.ds.height() != last.ds.height() {
            frame.append("\x1b[r"); // reset scrolling region
            frame.append("\x1b[0m\x1b[H\x1b[2J"); // clear screen
            initialized = false;
            frame.cursor_x = 0;
            frame.cursor_y = 0;
            frame.current_rendition = initial_rendition();
            frame.current_hyperlink = Hyperlink::default();
        } else {
            frame.cursor_x = last.ds.cursor_col();
            frame.cursor_y = last.ds.cursor_row();
            frame.current_rendition = *last.ds.renditions();
            frame.current_hyperlink = last.ds.hyperlink().clone();
        }

        // Is cursor visibility known?
        if !initialized {
            frame.cursor_visible = false;
            frame.append("\x1b[?25l");
        }

        let mut frame_y = 0;
        let mut blank_row: Option<Rc<Row>> = None;
        let mut rows: Vec<Rc<Row>> = last.rows().to_vec();

        // Widen our copy of the old rows if the screen grew wider.
        if last.ds.width() < f.ds.width() {
            let bg: ColorType = f.ds.background_rendition();
            for p in rows.iter_mut() {
                let row = Rc::make_mut(p);
                row.cells.resize(f.ds.width() as usize, Cell::new(bg));
            }
        }
        // Add rows if the screen grew taller.
        if (rows.len() as i32) < f.ds.height() {
            let blank = Rc::new(Row::new(f.ds.width() as usize, 0));
            rows.resize(f.ds.height() as usize, blank.clone());
            blank_row = Some(blank);
        }

        // Shortcut: has the display simply moved up by some number of lines?
        if initialized {
            let mut lines_scrolled = 0;
            let mut scroll_height = 0;

            for row in 0..f.ds.height() {
                let new_row = f.row(0).expect("row 0 exists");
                let old_row = &rows[row as usize];
                if !(std::ptr::eq(new_row, old_row.as_ref()) || *new_row == **old_row) {
                    continue;
                }
                // Row 0 matching means we are looking at ourselves, not a scroll.
                if row == 0 {
                    break;
                }
                lines_scrolled = row;
                scroll_height = 1;

                // How tall is the region that scrolled?
                let mut region_height = 1;
                while lines_scrolled + region_height < f.ds.height() {
                    if *f.row(region_height).expect("row exists")
                        == *rows[(lines_scrolled + region_height) as usize]
                    {
                        scroll_height = region_height + 1;
                    } else {
                        break;
                    }
                    region_height += 1;
                }
                break;
            }

            if scroll_height != 0 {
                frame_y = scroll_height;

                if lines_scrolled != 0 {
                    let blank = blank_row
                        .get_or_insert_with(|| Rc::new(Row::new(f.ds.width() as usize, 0)))
                        .clone();

                    frame.update_rendition(&initial_rendition(), true);
                    frame.update_hyperlink(&Hyperlink::default(), true);

                    let top_margin = 0;
                    let bottom_margin = top_margin + lines_scrolled + scroll_height - 1;
                    debug_assert!(bottom_margin < f.ds.height());

                    // Common case: already on the bottom line and scrolling the whole
                    // screen, so a carriage return and some line feeds will do.
                    if scroll_height + lines_scrolled == f.ds.height()
                        && frame.cursor_y + 1 == f.ds.height()
                    {
                        frame.append_char('\r');
                        frame.append_repeated(lines_scrolled as usize, '\n');
                        frame.cursor_x = 0;
                    } else {
                        frame.append(&format!("\x1b[{};{}r", top_margin + 1, bottom_margin + 1));

                        // Go to the bottom of the scrolling region.
                        frame.cursor_x = -1;
                        frame.cursor_y = -1;
                        frame.append_silent_move(bottom_margin, 0);

                        frame.append_repeated(lines_scrolled as usize, '\n');

                        frame.append("\x1b[r");
                        // The cursor position is unknown after unsetting the region.
                        frame.cursor_x = -1;
                        frame.cursor_y = -1;
                    }

                    // Mirror the scroll in our local copy of the old rows.
                    for i in top_margin..=bottom_margin {
                        rows[i as usize] = if i + lines_scrolled <= bottom_margin {
                            rows[(i + lines_scrolled) as usize].clone()
                        } else {
                            blank.clone()
                        };
                    }
                }
            }
        }

        // Update the display row by row.
        let mut wrap = false;
        while frame_y < f.ds.height() {
            wrap = self.put_row(
                initialized,
                &mut frame,
                f,
                frame_y,
                &rows[frame_y as usize].clone(),
                wrap,
            );
            frame_y += 1;
        }

        // Has the cursor moved?
        if !initialized
            || f.ds.cursor_row() != frame.cursor_y
            || f.ds.cursor_col() != frame.cursor_x
        {
            frame.append_move(f.ds.cursor_row(), f.ds.cursor_col());
        }

        // Has cursor visibility changed?
        if !initialized || f.ds.cursor_visible != frame.cursor_visible {
            frame.append(if f.ds.cursor_visible {
                "\x1b[?25h"
            } else {
                "\x1b[?25l"
            });
        }

        frame.update_rendition(f.ds.renditions(), !initialized);
        frame.update_hyperlink(f.ds.hyperlink(), !initialized);

        // Has bracketed paste changed?
        if !initialized || f.ds.bracketed_paste != last.ds.bracketed_paste {
            frame.append(if f.ds.bracketed_paste {
                "\x1b[?2004h"
            } else {
                "\x1b[?2004l"
            });
        }

        // Has mouse reporting changed?
        if !initialized || f.ds.mouse_reporting_mode != last.ds.mouse_reporting_mode {
            if f.ds.mouse_reporting_mode == MouseReportingMode::None {
                frame.append("\x1b[?1003l");
                frame.append("\x1b[?1002l");
                frame.append("\x1b[?1001l");
                frame.append("\x1b[?1000l");
            } else {
                if last.ds.mouse_reporting_mode != MouseReportingMode::None {
                    frame.append(&format!("\x1b[?{}l", last.ds.mouse_reporting_mode as i32));
                }
                frame.append(&format!("\x1b[?{}h", f.ds.mouse_reporting_mode as i32));
            }
        }

        // Has mouse focus reporting changed?
        if !initialized || f.ds.mouse_focus_event != last.ds.mouse_focus_event {
            frame.append(if f.ds.mouse_focus_event {
                "\x1b[?1004h"
            } else {
                "\x1b[?1004l"
            });
        }

        // Has mouse encoding changed?
        if !initialized || f.ds.mouse_encoding_mode != last.ds.mouse_encoding_mode {
            if f.ds.mouse_encoding_mode == MouseEncodingMode::Default {
                frame.append("\x1b[?1015l");
                frame.append("\x1b[?1006l");
                frame.append("\x1b[?1005l");
            } else {
                if last.ds.mouse_encoding_mode != MouseEncodingMode::Default {
                    frame.append(&format!("\x1b[?{}l", last.ds.mouse_encoding_mode as i32));
                }
                frame.append(&format!("\x1b[?{}h", f.ds.mouse_encoding_mode as i32));
            }
        }

        frame.str
    }

    /// Draw one row. Returns whether the next row must start by writing its first column.
    fn put_row(
        &self,
        initialized: bool,
        frame: &mut FrameState,
        f: &Framebuffer,
        frame_y: i32,
        old_row: &Rc<Row>,
        wrap: bool,
    ) -> bool {
        let mut frame_x = 0i32;

        let row = f.rows()[frame_y as usize].clone();
        let cells = &row.cells;
        let old_cells = &old_row.cells;

        // A wrap from the previous row forces the first column to be written.
        if wrap {
            let cell = &cells[0];
            frame.update_rendition(cell.renditions(), false);
            frame.update_hyperlink(cell.hyperlink(), false);
            frame.append_cell(cell);
            frame_x += cell.width() as i32;
            frame.cursor_x += cell.width() as i32;
        }

        // Same row object? Nothing at all to do.
        if initialized && Rc::ptr_eq(&row, old_row) {
            return false;
        }

        let wrap_this = row.wrap();
        let row_width = f.ds.width();
        let mut clear_count = 0i32;
        let mut wrote_last_cell = false;
        let mut blank_renditions = initial_rendition();
        let mut blank_hyperlink = Hyperlink::default();

        while frame_x < row_width {
            let cell = &cells[frame_x as usize];

            // Unchanged cell with nothing pending? Skip it.
            if initialized
                && clear_count == 0
                && old_cells
                    .get(frame_x as usize)
                    .map(|c| cell == c)
                    .unwrap_or(false)
            {
                frame_x += cell.width() as i32;
                continue;
            }

            // Gather a run of empty cells so they can be erased in one go.
            if cell.is_empty() {
                if clear_count == 0 {
                    blank_renditions = *cell.renditions();
                    blank_hyperlink = cell.hyperlink().clone();
                }
                if *cell.renditions() == blank_renditions && *cell.hyperlink() == blank_hyperlink {
                    clear_count += 1;
                    frame_x += 1;
                    continue;
                }
            }

            // Flush a pending run of blanks that ends before the end of the row.
            if clear_count != 0 {
                frame.append_silent_move(frame_y, frame_x - clear_count);
                frame.update_rendition(&blank_renditions, false);
                frame.update_hyperlink(&blank_hyperlink, false);
                if self.can_use_erase(frame) && self.has_ech && clear_count > 4 {
                    frame.append(&format!("\x1b[{clear_count}X"));
                } else {
                    frame.append_repeated(clear_count as usize, ' ');
                    frame.cursor_x = frame_x;
                }
                clear_count = 0;

                // Another empty cell in a different rendition starts a fresh run.
                if cell.is_empty() {
                    blank_renditions = *cell.renditions();
                    blank_hyperlink = cell.hyperlink().clone();
                    clear_count = 1;
                    frame_x += 1;
                    continue;
                }
            }

            let cell_width = cell.width() as i32;
            // About to print the last character of a wrapping row: throw away the known
            // cursor position to force explicit positioning. Our input terminal state may
            // have the cursor on the autowrap column ("column 81") while our output
            // states always snap it to the true last column, and the diff has to apply to
            // either for verification to work.
            if wrap_this && frame_x + cell_width >= row_width {
                frame.cursor_x = -1;
                frame.cursor_y = -1;
            }
            frame.append_silent_move(frame_y, frame_x);
            frame.update_rendition(cell.renditions(), false);
            frame.update_hyperlink(cell.hyperlink(), false);
            frame.append_cell(cell);
            frame_x += cell_width;
            frame.cursor_x += cell_width;
            if frame_x >= row_width {
                wrote_last_cell = true;
            }
        }

        // Flush a run of blanks that reaches the end of the line.
        if clear_count != 0 {
            frame.append_silent_move(frame_y, frame_x - clear_count);
            frame.update_rendition(&blank_renditions, false);
            frame.update_hyperlink(&blank_hyperlink, false);

            if self.can_use_erase(frame) && !wrap_this {
                frame.append("\x1b[K");
            } else {
                frame.append_repeated(clear_count as usize, ' ');
                frame.cursor_x = frame_x;
                wrote_last_cell = true;
            }
        }

        if !(wrote_last_cell && frame_y < f.ds.height() - 1) {
            return false;
        }

        // Let the real cursor wrap where it wrapped for us, so that a word-select in the
        // user's terminal groups the end of one line with the start of the next.
        if wrap_this {
            frame.cursor_x = 0;
            frame.cursor_y += 1;
            return true;
        }

        frame.append("\r\n");
        frame.cursor_x = 0;
        frame.cursor_y += 1;
        false
    }
}

fn bytes_to_string(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::Emulator;

    /// A display that assumes a capable terminal, matching `Display(false)` in the C++.
    fn display() -> Display {
        Display::new(false).expect("no environment needed")
    }

    #[test]
    fn an_unchanged_screen_produces_almost_nothing() {
        let emu = Emulator::new(20, 4);
        let d = display();
        let frame = d.new_frame(true, emu.fb(), emu.fb());
        assert!(
            frame.is_empty(),
            "an identical screen still emitted {frame:?}"
        );
    }

    #[test]
    fn an_uninitialised_frame_clears_and_repaints() {
        let mut emu = Emulator::new(10, 2);
        emu.input(b"hi");
        let d = display();
        let frame = d.new_frame(false, &Framebuffer::new(10, 2), emu.fb());
        assert!(frame.contains("\x1b[2J"), "no clear screen: {frame:?}");
        assert!(frame.contains("hi"), "content missing: {frame:?}");
    }

    #[test]
    fn a_single_changed_cell_is_a_small_diff() {
        let mut before = Emulator::new(20, 4);
        before.input(b"hello");
        let mut after = before.clone();
        after.input(b"!");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains('!'), "the change is missing: {frame:?}");
        // The unchanged text must not be repainted.
        assert!(
            !frame.contains("hello"),
            "repainted the whole row: {frame:?}"
        );
    }

    #[test]
    fn a_size_change_forces_a_full_repaint() {
        let mut before = Emulator::new(10, 2);
        before.input(b"hi");
        let mut after = before.clone();
        after.resize(20, 4);

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b[r"), "scrolling region not reset");
        assert!(frame.contains("\x1b[2J"), "screen not cleared");
    }

    #[test]
    fn the_bell_is_forwarded_once() {
        let mut before = Emulator::new(10, 2);
        let mut after = before.clone();
        after.input(b"\x07");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert_eq!(frame.matches('\u{7}').count(), 1);

        // Ringing it again produces another bell.
        before = after.clone();
        let mut after2 = after.clone();
        after2.input(b"\x07");
        let frame = d.new_frame(true, before.fb(), after2.fb());
        assert_eq!(frame.matches('\u{7}').count(), 1);
    }

    #[test]
    fn a_title_change_is_sent_as_one_sequence_when_both_match() {
        let mut before = Emulator::new(10, 2);
        before.input(b"x");
        let mut after = before.clone();
        after.input(b"\x1b]0;hi\x07");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b]0;hi\u{7}"), "{frame:?}");
    }

    #[test]
    fn separate_icon_and_title_are_sent_separately() {
        let mut before = Emulator::new(10, 2);
        before.input(b"x");
        let mut after = before.clone();
        after.input(b"\x1b]1;icon\x07");
        after.input(b"\x1b]2;title\x07");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b]1;icon\u{7}"), "{frame:?}");
        assert!(frame.contains("\x1b]2;title\u{7}"), "{frame:?}");
    }

    #[test]
    fn the_clipboard_is_not_replayed_on_a_fresh_attach() {
        let mut before = Emulator::new(10, 2);
        let mut after = before.clone();
        after.input(b"\x1b]52;c;aGk=\x07");

        let d = display();
        // Uninitialised means a client is attaching; a stale copy must not be replayed.
        let frame = d.new_frame(false, before.fb(), after.fb());
        assert!(
            !frame.contains("\x1b]52;"),
            "replayed the clipboard: {frame:?}"
        );

        // Once initialised, the change is forwarded.
        before = Emulator::new(10, 2);
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b]52;c;aGk=\u{7}"), "{frame:?}");
    }

    #[test]
    fn colour_queries_are_not_replayed_on_a_fresh_attach() {
        let before = Emulator::new(10, 2);
        let mut after = before.clone();
        after.input(b"\x1b]4;1;?\x07");

        let d = display();
        let frame = d.new_frame(false, before.fb(), after.fb());
        assert!(!frame.contains("\x1b]4;1;?"), "{frame:?}");

        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b]4;1;?\x1b\\"), "{frame:?}");
    }

    #[test]
    fn mode_changes_are_emitted() {
        let before = Emulator::new(10, 2);

        let d = display();
        for (input, expected) in [
            (&b"\x1b[?2004h"[..], "\x1b[?2004h"),
            (b"\x1b[?5h", "\x1b[?5h"),
            (b"\x1b[?1004h", "\x1b[?1004h"),
            (b"\x1b[?1000h", "\x1b[?1000h"),
            (b"\x1b[?1006h", "\x1b[?1006h"),
        ] {
            let mut after = before.clone();
            after.input(input);
            let frame = d.new_frame(true, before.fb(), after.fb());
            assert!(
                frame.contains(expected),
                "{expected:?} missing from {frame:?}"
            );
        }
    }

    #[test]
    fn cursor_visibility_changes_are_emitted() {
        let before = Emulator::new(10, 2);
        let mut after = before.clone();
        after.input(b"\x1b[?25l");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        assert!(frame.contains("\x1b[?25l"), "{frame:?}");
    }

    #[test]
    fn short_cursor_moves_prefer_backspaces_and_newlines() {
        let mut frame = FrameState::new(&Framebuffer::new(80, 24));
        frame.cursor_x = 5;
        frame.cursor_y = 0;
        frame.append_move(0, 3);
        assert_eq!(
            frame.str, "\u{8}\u{8}",
            "a short move left should backspace"
        );

        let mut frame = FrameState::new(&Framebuffer::new(80, 24));
        frame.cursor_x = 5;
        frame.cursor_y = 0;
        frame.append_move(2, 0);
        assert_eq!(frame.str, "\r\n\n", "a move to column 0 should use CR/LF");

        // A long move is worth an absolute position.
        let mut frame = FrameState::new(&Framebuffer::new(80, 24));
        frame.cursor_x = 0;
        frame.cursor_y = 0;
        frame.append_move(10, 40);
        assert_eq!(frame.str, "\x1b[11;41H");
    }

    #[test]
    fn an_unknown_cursor_position_forces_absolute_addressing() {
        let mut frame = FrameState::new(&Framebuffer::new(80, 24));
        frame.cursor_x = -1;
        frame.cursor_y = -1;
        frame.append_move(0, 0);
        assert_eq!(frame.str, "\x1b[1;1H");
    }

    #[test]
    fn scrolling_is_rendered_as_line_feeds_not_a_repaint() {
        let mut before = Emulator::new(10, 3);
        before.input(b"aaa\r\nbbb\r\nccc");
        let mut after = before.clone();
        after.input(b"\r\nddd");

        let d = display();
        let frame = d.new_frame(true, before.fb(), after.fb());
        // The scroll shortcut should avoid repainting the surviving rows.
        assert!(
            !frame.contains("bbb"),
            "repainted a scrolled row: {frame:?}"
        );
        assert!(frame.contains("ddd"), "new row missing: {frame:?}");
    }

    #[test]
    fn open_and_close_bracket_the_session() {
        let d = display();
        assert!(d.open().contains("\x1b[?1h"));
        let close = d.close();
        assert!(close.contains("\x1b[?1l"));
        assert!(close.contains("\x1b[?25h"), "cursor left hidden");
        assert!(close.contains("\x1b[0m"), "renditions left set");
    }
}
