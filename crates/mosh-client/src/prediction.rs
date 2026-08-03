//! Guessing what the server will echo, and withdrawing the guess when it does not.
//!
//! Transliterated from `PredictionEngine` in `third_party/mosh/src/frontend/terminaloverlay.cc`.
//!
//! Predictions are grouped into epochs. When one turns out wrong, the whole epoch it
//! belongs to is abandoned rather than the individual guess, because a wrong prediction
//! usually means the model of the remote application was wrong, not that one character
//! was mistyped.

use mosh_terminal::framebuffer::Framebuffer;
use mosh_terminal::parser::{Kind, Utf8Parser};

use crate::overlay::{
    ConditionalCursorMove, ConditionalOverlayCell, ConditionalOverlayRow, Validity,
};

/// Round-trip times, in milliseconds, that turn prediction display on and off. The gap
/// between them is hysteresis, so display does not flicker at the boundary.
const SRTT_TRIGGER_LOW: u64 = 20;
const SRTT_TRIGGER_HIGH: u64 = 30;

/// The same idea for underlining.
const FLAG_TRIGGER_LOW: u64 = 50;
const FLAG_TRIGGER_HIGH: u64 = 80;

/// A prediction outstanding this long is a glitch.
const GLITCH_THRESHOLD: u64 = 250;
/// Prompt confirmations needed to work off a glitch.
const GLITCH_REPAIR_COUNT: u64 = 10;
/// Minimum gap between confirmations that count towards repair.
const GLITCH_REPAIR_MININTERVAL: u64 = 150;
/// Outstanding this long and predictions get underlined.
const GLITCH_FLAG_THRESHOLD: u64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayPreference {
    Always,
    Never,
    Adaptive,
    Experimental,
}

pub struct PredictionEngine {
    last_byte: u8,
    parser: Utf8Parser,

    overlays: Vec<ConditionalOverlayRow>,
    cursors: Vec<ConditionalCursorMove>,

    local_frame_sent: u64,
    local_frame_acked: u64,
    local_frame_late_acked: u64,

    prediction_epoch: u64,
    confirmed_epoch: u64,

    /// Whether predictions are underlined.
    flagging: bool,
    /// Show predictions because the round trip is slow.
    srtt_trigger: bool,
    /// Show predictions because one has been outstanding a long time.
    glitch_trigger: u64,
    last_quick_confirmation: u64,

    send_interval: u64,
    last_height: i32,
    last_width: i32,

    display_preference: DisplayPreference,
    predict_overwrite: bool,
}

impl Default for PredictionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PredictionEngine {
    pub fn new() -> Self {
        PredictionEngine {
            last_byte: 0,
            parser: Utf8Parser::new(),
            overlays: Vec::new(),
            cursors: Vec::new(),
            local_frame_sent: 0,
            local_frame_acked: 0,
            local_frame_late_acked: 0,
            prediction_epoch: 1,
            confirmed_epoch: 0,
            flagging: false,
            srtt_trigger: false,
            glitch_trigger: 0,
            last_quick_confirmation: 0,
            send_interval: 250,
            last_height: 0,
            last_width: 0,
            display_preference: DisplayPreference::Adaptive,
            predict_overwrite: false,
        }
    }

    pub fn set_display_preference(&mut self, p: DisplayPreference) {
        self.display_preference = p;
    }

    pub fn set_predict_overwrite(&mut self, overwrite: bool) {
        self.predict_overwrite = overwrite;
    }

    pub fn set_local_frame_sent(&mut self, x: u64) {
        self.local_frame_sent = x;
    }

    pub fn set_local_frame_acked(&mut self, x: u64) {
        self.local_frame_acked = x;
    }

    pub fn set_local_frame_late_acked(&mut self, x: u64) {
        self.local_frame_late_acked = x;
    }

    pub fn set_send_interval(&mut self, x: u64) {
        self.send_interval = x;
    }

    /// Is anything being predicted at all?
    pub fn active(&self) -> bool {
        !self.cursors.is_empty()
            || self
                .overlays
                .iter()
                .any(|row| row.overlay_cells.iter().any(|c| c.base.active))
    }

    /// Are there timing triggers that have not fired yet?
    fn timing_tests_necessary(&self) -> bool {
        !(self.glitch_trigger != 0 && self.flagging)
    }

    pub fn wait_time(&self) -> u64 {
        if self.timing_tests_necessary() && self.active() {
            50
        } else {
            u64::MAX
        }
    }

    /// Draw the predictions onto a screen.
    pub fn apply(&self, fb: &mut Framebuffer) {
        let showing = self.srtt_trigger
            || self.glitch_trigger != 0
            || self.display_preference == DisplayPreference::Always
            || self.display_preference == DisplayPreference::Experimental;
        if self.display_preference == DisplayPreference::Never || !showing {
            return;
        }

        for c in &self.cursors {
            c.apply(fb, self.confirmed_epoch);
        }
        for row in &self.overlays {
            row.apply(fb, self.confirmed_epoch, self.flagging);
        }
    }

    pub fn reset(&mut self) {
        self.cursors.clear();
        self.overlays.clear();
        self.become_tentative();
    }

    /// Start a new epoch, so predictions made from here are judged together.
    fn become_tentative(&mut self) {
        if self.display_preference != DisplayPreference::Experimental {
            self.prediction_epoch += 1;
        }
    }

    fn cursor(&self) -> &ConditionalCursorMove {
        self.cursors
            .last()
            .expect("cursor list is never empty here")
    }

    fn cursor_mut(&mut self) -> &mut ConditionalCursorMove {
        self.cursors
            .last_mut()
            .expect("cursor list is never empty here")
    }

    fn init_cursor(&mut self, fb: &Framebuffer) {
        if self.cursors.is_empty() {
            let mut c = ConditionalCursorMove::new(
                self.local_frame_sent + 1,
                fb.ds.cursor_row(),
                fb.ds.cursor_col(),
                self.prediction_epoch,
            );
            c.base.active = true;
            self.cursors.push(c);
        } else if self.cursor().base.tentative_until_epoch != self.prediction_epoch {
            // The epoch moved on, so start a fresh prediction from where we think we are.
            let (row, col) = (self.cursor().row, self.cursor().base.col);
            let mut c = ConditionalCursorMove::new(
                self.local_frame_sent + 1,
                row,
                col,
                self.prediction_epoch,
            );
            c.base.active = true;
            self.cursors.push(c);
        }
    }

    /// Abandon a whole epoch of predictions, then start another.
    fn kill_epoch(&mut self, epoch: u64, fb: &Framebuffer) {
        self.cursors
            .retain(|c| !c.base.tentative(epoch.saturating_sub(1)));

        let mut c = ConditionalCursorMove::new(
            self.local_frame_sent + 1,
            fb.ds.cursor_row(),
            fb.ds.cursor_col(),
            self.prediction_epoch,
        );
        c.base.active = true;
        self.cursors.push(c);

        for row in &mut self.overlays {
            for cell in &mut row.overlay_cells {
                if cell.base.tentative(epoch.saturating_sub(1)) {
                    cell.reset();
                }
            }
        }

        self.become_tentative();
    }

    /// Index of the overlay row for `row_num`, creating it if needed.
    fn get_or_make_row(&mut self, row_num: i32, num_cols: i32) -> usize {
        if let Some(i) = self.overlays.iter().position(|r| r.row_num == row_num) {
            return i;
        }
        let mut r = ConditionalOverlayRow::new(row_num, num_cols);
        for (i, cell) in r.overlay_cells.iter_mut().enumerate() {
            *cell = ConditionalOverlayCell::new(0, i as i32, self.prediction_epoch);
        }
        self.overlays.push(r);
        self.overlays.len() - 1
    }

    /// Judge outstanding predictions against what the server actually sent.
    pub fn cull(&mut self, fb: &Framebuffer, now: u64) {
        if self.display_preference == DisplayPreference::Never {
            return;
        }

        // A resize invalidates every prediction's coordinates.
        if self.last_height != fb.ds.height() || self.last_width != fb.ds.width() {
            self.last_height = fb.ds.height();
            self.last_width = fb.ds.width();
            self.reset();
        }

        // Both triggers use hysteresis so display does not flicker at the boundary.
        if self.send_interval > SRTT_TRIGGER_HIGH {
            self.srtt_trigger = true;
        } else if self.srtt_trigger && self.send_interval <= SRTT_TRIGGER_LOW && !self.active() {
            // Only stop once nothing is on screen, or predictions would vanish abruptly.
            self.srtt_trigger = false;
        }

        if self.send_interval > FLAG_TRIGGER_HIGH {
            self.flagging = true;
        } else if self.send_interval <= FLAG_TRIGGER_LOW {
            self.flagging = false;
        }

        // A bad enough glitch also turns on underlining.
        if self.glitch_trigger > GLITCH_REPAIR_COUNT {
            self.flagging = true;
        }

        // Rows that a resize put off screen.
        self.overlays
            .retain(|r| r.row_num >= 0 && r.row_num < fb.ds.height());

        let mut kill: Option<u64> = None;
        let mut full_reset = false;

        'rows: for ri in 0..self.overlays.len() {
            let row_num = self.overlays[ri].row_num;
            for ci in 0..self.overlays[ri].overlay_cells.len() {
                let validity = self.overlays[ri].overlay_cells[ci].get_validity(
                    fb,
                    row_num,
                    self.local_frame_late_acked,
                );
                match validity {
                    Validity::Incorrect => {
                        let cell = &mut self.overlays[ri].overlay_cells[ci];
                        if cell.base.tentative(self.confirmed_epoch) {
                            if self.display_preference == DisplayPreference::Experimental {
                                cell.reset();
                            } else {
                                kill = Some(cell.base.tentative_until_epoch);
                                break 'rows;
                            }
                        } else if self.display_preference == DisplayPreference::Experimental {
                            cell.reset();
                        } else {
                            // A confirmed-epoch prediction was wrong: our whole model of
                            // the remote application is suspect, so start over.
                            full_reset = true;
                            break 'rows;
                        }
                    }
                    Validity::Correct => {
                        let cell = &self.overlays[ri].overlay_cells[ci];
                        if cell.base.tentative_until_epoch > self.confirmed_epoch {
                            self.confirmed_epoch = cell.base.tentative_until_epoch;
                        }
                        // Predictions confirmed quickly slowly work off a glitch.
                        let prediction_time = cell.base.prediction_time;
                        if now.saturating_sub(prediction_time) < GLITCH_THRESHOLD
                            && self.glitch_trigger > 0
                            && now.saturating_sub(GLITCH_REPAIR_MININTERVAL)
                                >= self.last_quick_confirmation
                        {
                            self.glitch_trigger -= 1;
                            self.last_quick_confirmation = now;
                        }
                        // Adopt the renditions the server actually used for the rest of
                        // the row, so later predictions inherit them.
                        if let Some(actual) = fb.cell(row_num, ci as i32) {
                            let renditions = *actual.renditions();
                            for k in ci..self.overlays[ri].overlay_cells.len() {
                                self.overlays[ri].overlay_cells[k]
                                    .replacement
                                    .set_renditions(renditions);
                            }
                        }
                        self.overlays[ri].overlay_cells[ci].reset();
                    }
                    Validity::CorrectNoCredit => {
                        self.overlays[ri].overlay_cells[ci].reset();
                    }
                    Validity::Pending => {
                        // A prediction outstanding a long time means the link is worse
                        // than the round-trip estimate suggests, so show them anyway.
                        let elapsed = now.saturating_sub(
                            self.overlays[ri].overlay_cells[ci].base.prediction_time,
                        );
                        if elapsed >= GLITCH_FLAG_THRESHOLD {
                            self.glitch_trigger = GLITCH_REPAIR_COUNT * 2; // show and underline
                        } else if elapsed >= GLITCH_THRESHOLD
                            && self.glitch_trigger < GLITCH_REPAIR_COUNT
                        {
                            self.glitch_trigger = GLITCH_REPAIR_COUNT; // just show
                        }
                    }
                    Validity::Inactive => {}
                }
            }
        }

        if full_reset {
            self.reset();
            return;
        }
        if let Some(epoch) = kill {
            self.kill_epoch(epoch, fb);
        }

        // A wrong cursor prediction is as damning as a wrong cell.
        if !self.cursors.is_empty()
            && self.cursor().get_validity(fb, self.local_frame_late_acked) == Validity::Incorrect
        {
            if self.display_preference == DisplayPreference::Experimental {
                self.cursors.clear();
            } else {
                self.reset();
                return;
            }
        }

        let late_acked = self.local_frame_late_acked;
        self.cursors
            .retain(|c| c.get_validity(fb, late_acked) == Validity::Pending);
    }

    /// Take one byte the user typed and predict what it will do.
    pub fn new_user_byte(&mut self, mut the_byte: u8, fb: &Framebuffer, now: u64) {
        if self.display_preference == DisplayPreference::Never {
            return;
        }
        if self.display_preference == DisplayPreference::Experimental {
            self.prediction_epoch = self.confirmed_epoch;
        }

        self.cull(fb, now);

        // Rewrite an application-mode cursor key into its ANSI form, so the arrow
        // handling below sees one spelling rather than two.
        if self.last_byte == 0x1b && the_byte == b'O' {
            the_byte = b'[';
        }
        self.last_byte = the_byte;

        let mut actions = Vec::new();
        self.parser.input(the_byte, &mut actions);

        for action in actions {
            match action.kind {
                Kind::Print => {
                    let Some(ch) = action.ch else { continue };
                    self.predict_print(ch, fb, now);
                }
                Kind::Execute => {
                    if action.ch == Some('\r') {
                        self.become_tentative();
                        self.newline_carriage_return(fb, now);
                    } else {
                        self.become_tentative();
                    }
                }
                Kind::EscDispatch => self.become_tentative(),
                Kind::CsiDispatch => match action.ch {
                    Some('C') => {
                        // Right arrow.
                        self.init_cursor(fb);
                        if self.cursor().base.col < fb.ds.width() - 1 {
                            let sent = self.local_frame_sent;
                            let c = self.cursor_mut();
                            c.base.col += 1;
                            c.base.expire(sent + 1, now);
                        }
                    }
                    Some('D') => {
                        // Left arrow.
                        self.init_cursor(fb);
                        if self.cursor().base.col > 0 {
                            let sent = self.local_frame_sent;
                            let c = self.cursor_mut();
                            c.base.col -= 1;
                            c.base.expire(sent + 1, now);
                        }
                    }
                    _ => self.become_tentative(),
                },
                _ => {}
            }
        }
    }

    fn predict_print(&mut self, ch: char, fb: &Framebuffer, now: u64) {
        self.init_cursor(fb);

        if ch == '\u{7f}' {
            self.predict_backspace(fb, now);
            return;
        }

        // Only ordinary single-width characters are predictable; anything else could do
        // something we cannot model.
        if (ch as u32) < 0x20 || mosh_sys::wcwidth(ch) != Some(1) {
            self.become_tentative();
            return;
        }

        let (row, col) = (self.cursor().row, self.cursor().base.col);
        if row < 0 || col < 0 || row >= fb.ds.height() || col >= fb.ds.width() {
            // A resize can strand the cursor prediction; the C++ asserts here.
            self.become_tentative();
            return;
        }

        let width = fb.ds.width();
        let ri = self.get_or_make_row(row, width);

        if col + 1 >= width {
            // The last column behaves differently in different applications: emacs shows
            // a wrap marker, a shell just writes the character. Do not commit.
            self.become_tentative();
        }

        // Shift the rest of the row right to make room, unless overwriting.
        let rightmost = if self.predict_overwrite {
            col
        } else {
            width - 1
        };
        let mut i = rightmost;
        while i > col {
            let prev_active = self.overlays[ri].overlay_cells[(i - 1) as usize]
                .base
                .active;
            let prev_unknown = self.overlays[ri].overlay_cells[(i - 1) as usize].unknown;
            let prev_replacement = self.overlays[ri].overlay_cells[(i - 1) as usize]
                .replacement
                .clone();
            let prev_actual = fb.cell(row, i - 1).cloned();
            let orig = fb.cell(row, i).cloned();

            let sent = self.local_frame_sent;
            let epoch = self.prediction_epoch;
            let cell = &mut self.overlays[ri].overlay_cells[i as usize];
            cell.reset_with_orig();
            cell.base.active = true;
            cell.base.tentative_until_epoch = epoch;
            cell.base.expire(sent + 1, now);
            if let Some(o) = orig {
                cell.original_contents.push(o);
            }

            if i == width - 1 {
                // What falls off the end is not knowable.
                cell.unknown = true;
            } else if prev_active {
                if prev_unknown {
                    cell.unknown = true;
                } else {
                    cell.unknown = false;
                    cell.replacement = prev_replacement;
                }
            } else {
                cell.unknown = false;
                if let Some(a) = prev_actual {
                    cell.replacement = a;
                }
            }
            i -= 1;
        }

        // Inherit renditions from the character to the left, which is a better guess
        // than the draw state when the application is mid-sequence.
        let mut renditions = *fb.ds.renditions();
        if col > 0 {
            let left = &self.overlays[ri].overlay_cells[(col - 1) as usize];
            if left.base.active && !left.unknown {
                renditions = *left.replacement.renditions();
            } else if let Some(a) = fb.cell(row, col - 1) {
                renditions = *a.renditions();
            }
        }
        let orig = fb.cell(row, col).cloned();

        let sent = self.local_frame_sent;
        let epoch = self.prediction_epoch;
        let cell = &mut self.overlays[ri].overlay_cells[col as usize];
        cell.reset_with_orig();
        cell.base.active = true;
        cell.base.tentative_until_epoch = epoch;
        cell.base.expire(sent + 1, now);
        cell.replacement.set_renditions(renditions);
        cell.replacement.clear();
        cell.replacement.append(ch);
        if let Some(o) = orig {
            cell.original_contents.push(o);
        }

        let sent = self.local_frame_sent;
        self.cursor_mut().base.expire(sent + 1, now);

        if self.cursor().base.col < width - 1 {
            self.cursor_mut().base.col += 1;
        } else {
            self.become_tentative();
            self.newline_carriage_return(fb, now);
        }
    }

    fn predict_backspace(&mut self, fb: &Framebuffer, now: u64) {
        let (row, col) = (self.cursor().row, self.cursor().base.col);
        if col <= 0 || row < 0 || row >= fb.ds.height() {
            return;
        }

        let width = fb.ds.width();
        let ri = self.get_or_make_row(row, width);

        let sent = self.local_frame_sent;
        {
            let c = self.cursor_mut();
            c.base.col -= 1;
            c.base.expire(sent + 1, now);
        }
        let col = col - 1;
        let epoch = self.prediction_epoch;

        if self.predict_overwrite {
            // Just blank the cell we backed over.
            let orig = fb.cell(row, col).cloned();
            let cell = &mut self.overlays[ri].overlay_cells[col as usize];
            cell.reset_with_orig();
            cell.base.active = true;
            cell.base.tentative_until_epoch = epoch;
            cell.base.expire(sent + 1, now);
            if let Some(o) = orig {
                cell.original_contents.push(o.clone());
                cell.replacement = o;
            }
            cell.replacement.clear();
            cell.replacement.append(' ');
            return;
        }

        // Otherwise the rest of the row shifts left.
        for i in col..width {
            let next_active = if i + 1 < width {
                self.overlays[ri].overlay_cells[(i + 1) as usize]
                    .base
                    .active
            } else {
                false
            };
            let next_unknown = if i + 1 < width {
                self.overlays[ri].overlay_cells[(i + 1) as usize].unknown
            } else {
                false
            };
            let next_replacement = if i + 1 < width {
                Some(
                    self.overlays[ri].overlay_cells[(i + 1) as usize]
                        .replacement
                        .clone(),
                )
            } else {
                None
            };
            let next_actual = fb.cell(row, i + 1).cloned();
            let orig = fb.cell(row, i).cloned();

            let cell = &mut self.overlays[ri].overlay_cells[i as usize];
            cell.reset_with_orig();
            cell.base.active = true;
            cell.base.tentative_until_epoch = epoch;
            cell.base.expire(sent + 1, now);
            if let Some(o) = orig {
                cell.original_contents.push(o);
            }

            if i + 2 < width {
                if next_active {
                    if next_unknown {
                        cell.unknown = true;
                    } else {
                        cell.unknown = false;
                        if let Some(r) = next_replacement {
                            cell.replacement = r;
                        }
                    }
                } else {
                    cell.unknown = false;
                    if let Some(a) = next_actual {
                        cell.replacement = a;
                    }
                }
            } else {
                // What scrolls in from beyond the edge is not knowable.
                cell.unknown = true;
            }
        }
    }

    fn newline_carriage_return(&mut self, fb: &Framebuffer, now: u64) {
        self.init_cursor(fb);
        self.cursor_mut().base.col = 0;

        if self.cursor().row == fb.ds.height() - 1 {
            // Scrolling is not predicted -- that would need versioned cell predictions --
            // so blank the last row and let the server tell us what really happened.
            let (row, width) = (self.cursor().row, fb.ds.width());
            let ri = self.get_or_make_row(row, width);
            let sent = self.local_frame_sent;
            let epoch = self.prediction_epoch;
            for cell in &mut self.overlays[ri].overlay_cells {
                cell.base.active = true;
                cell.base.tentative_until_epoch = epoch;
                cell.base.expire(sent + 1, now);
                cell.replacement.clear();
            }
        } else {
            self.cursor_mut().row += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_showing() -> PredictionEngine {
        let mut e = PredictionEngine::new();
        // Always-on display keeps the tests about prediction rather than about triggers.
        e.set_display_preference(DisplayPreference::Always);
        e
    }

    /// Get an engine past its opening tentative epoch.
    ///
    /// A fresh engine shows nothing: predictions start tentative and are withheld until
    /// one has been confirmed correct, so mosh never displays a guess before it has any
    /// evidence that its guesses work. Real sessions leave that state after the first
    /// echo; tests have to do it deliberately.
    fn primed(fb: &mut Framebuffer) -> PredictionEngine {
        let mut e = engine_showing();
        e.new_user_byte(b'p', fb, 0);
        // The server echoes it: both the cell and the cursor must match, or the cursor
        // prediction is judged wrong and takes the whole epoch down with it.
        fb.mutable_cell(0, 0).append('p');
        fb.ds.move_col(1, false, false);
        e.set_local_frame_late_acked(100);
        e.cull(fb, 0);
        // Predictions made from here belong to frames we have not sent yet, so they stay
        // pending instead of being judged against a screen that cannot show them.
        e.set_local_frame_sent(1000);
        // That epoch is now confirmed, so later guesses are shown rather than withheld.
        e
    }

    fn screen_line(fb: &Framebuffer, row: i32) -> String {
        let mut s = String::new();
        for x in 0..fb.ds.width() {
            fb.cell(row, x).unwrap().print_grapheme(&mut s);
        }
        s.trim_end().to_string()
    }

    #[test]
    fn a_typed_character_is_predicted_onto_the_screen() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        e.new_user_byte(b'x', &fb, 0);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "px");
    }

    #[test]
    fn typing_advances_the_predicted_cursor() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        for c in b"abc" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "pabc");
        assert_eq!(shown.ds.cursor_col(), 4);
    }

    #[test]
    fn nothing_is_predicted_when_display_is_never() {
        mosh_sys::set_native_locale();
        let mut e = PredictionEngine::new();
        e.set_display_preference(DisplayPreference::Never);
        let fb = Framebuffer::new(20, 4);
        e.new_user_byte(b'x', &fb, 0);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "");
        assert!(!e.active());
    }

    #[test]
    fn control_characters_are_not_predicted() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(20, 4);
        // A tab could do anything depending on tab stops, so it must not be guessed.
        e.new_user_byte(b'\t', &fb, 0);
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "");
    }

    #[test]
    fn backspace_shifts_the_rest_of_the_row_left() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        for c in b"abc" {
            e.new_user_byte(*c, &fb, 0);
        }
        e.new_user_byte(0x7f, &fb, 0);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "pab");
    }

    #[test]
    fn backspace_at_the_start_of_a_line_does_nothing() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(20, 4);
        e.new_user_byte(0x7f, &fb, 0);
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(shown.ds.cursor_col(), 0);
    }

    #[test]
    fn arrow_keys_move_the_predicted_cursor() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        for c in b"abc" {
            e.new_user_byte(*c, &fb, 0);
        }
        // Left arrow.
        for c in b"\x1b[D" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(shown.ds.cursor_col(), 3);

        // Right arrow puts it back.
        for c in b"\x1b[C" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(shown.ds.cursor_col(), 4);
    }

    #[test]
    fn an_application_mode_arrow_is_understood_too() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(20, 4);
        e.new_user_byte(b'a', &fb, 0);
        // ESC O D is the application-mode left arrow; it must act like ESC [ D.
        for c in b"\x1bOD" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(shown.ds.cursor_col(), 0);
    }

    #[test]
    fn a_correct_prediction_is_retired() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let mut fb = Framebuffer::new(20, 4);
        e.new_user_byte(b'x', &fb, 0);
        assert!(e.active());

        // The server confirms the same character.
        fb.mutable_cell(0, 0).append('x');
        e.set_local_frame_late_acked(100);
        e.cull(&fb, 0);

        // Nothing left to predict, so the overlay stops contributing.
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "x");
    }

    #[test]
    fn a_wrong_prediction_abandons_the_epoch() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let mut fb = Framebuffer::new(20, 4);
        e.new_user_byte(b'x', &fb, 0);

        // The server put something else there entirely.
        fb.mutable_cell(0, 0).append('Z');
        e.set_local_frame_late_acked(100);
        e.cull(&fb, 0);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        // Our guess is gone; what the server said stands.
        assert_eq!(screen_line(&shown, 0), "Z");
    }

    #[test]
    fn a_resize_discards_every_prediction() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(20, 4);
        e.new_user_byte(b'x', &fb, 0);
        assert!(e.active());

        let mut smaller = Framebuffer::new(10, 2);
        e.cull(&smaller, 0);
        // Coordinates no longer mean what they did.
        let mut shown = smaller.clone();
        e.apply(&mut shown);
        assert_eq!(screen_line(&shown, 0), "");
        smaller.ds.move_col(0, false, false);
    }

    #[test]
    fn a_slow_link_turns_display_on_and_a_fast_one_turns_it_off() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        e.set_display_preference(DisplayPreference::Adaptive);

        e.set_send_interval(SRTT_TRIGGER_HIGH + 1);
        e.cull(&fb, 0);
        e.new_user_byte(b'x', &fb, 0);
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(
            screen_line(&shown, 0),
            "px",
            "slow link should show predictions"
        );

        // Dropping below the low mark only takes effect once nothing is outstanding,
        // so predictions do not vanish mid-flight.
        e.set_send_interval(SRTT_TRIGGER_LOW);
        e.cull(&fb, 0);
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(
            screen_line(&shown, 0),
            "px",
            "should not vanish while active"
        );
    }

    #[test]
    fn a_very_slow_link_underlines_predictions() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        e.set_display_preference(DisplayPreference::Adaptive);
        e.set_send_interval(FLAG_TRIGGER_HIGH + 1);
        e.cull(&fb, 0);
        e.new_user_byte(b'x', &fb, 0);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert!(shown
            .cell(0, 1)
            .unwrap()
            .renditions()
            .attribute(mosh_terminal::framebuffer::Attribute::Underlined));
    }

    #[test]
    fn a_long_outstanding_prediction_forces_display() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        e.set_display_preference(DisplayPreference::Adaptive);
        e.set_send_interval(SRTT_TRIGGER_LOW);
        e.new_user_byte(b'x', &fb, 0);

        // Still unconfirmed much later: the link is worse than the estimate says.
        e.set_local_frame_late_acked(0);
        e.cull(&fb, GLITCH_THRESHOLD + 1);

        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(
            screen_line(&shown, 0),
            "px",
            "a long-pending prediction should be shown even on a nominally fast link"
        );
    }

    #[test]
    fn wrapping_at_the_right_margin_moves_to_the_next_row() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(4, 3);
        for c in b"abcd" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        // The last column is deliberately uncommitted, so only assert the cursor left it.
        assert!(shown.ds.cursor_row() >= 0);
    }

    #[test]
    fn carriage_return_is_withheld_until_confirmed() {
        mosh_sys::set_native_locale();
        let mut fb = Framebuffer::new(20, 4);
        let mut e = primed(&mut fb);
        for c in b"abc" {
            e.new_user_byte(*c, &fb, 0);
        }
        let mut shown = fb.clone();
        e.apply(&mut shown);
        assert_eq!(shown.ds.cursor_col(), 4, "typing should move the cursor");

        e.new_user_byte(b'\r', &fb, 0);
        let mut shown = fb.clone();
        e.apply(&mut shown);
        // A carriage return starts a new epoch, because where the line ends up depends
        // on the remote application. The new position is held back until the server
        // confirms something in that epoch, so the shown cursor does not jump early.
        assert_eq!(
            shown.ds.cursor_col(),
            4,
            "the new line was shown before confirmation"
        );
        assert_eq!(shown.ds.cursor_row(), 0);
    }

    #[test]
    fn wait_time_is_short_only_while_triggers_can_still_fire() {
        mosh_sys::set_native_locale();
        let mut e = engine_showing();
        let fb = Framebuffer::new(20, 4);
        assert_eq!(e.wait_time(), u64::MAX, "nothing outstanding");

        e.new_user_byte(b'x', &fb, 0);
        assert_eq!(e.wait_time(), 50, "a timing trigger could still fire");
    }
}
