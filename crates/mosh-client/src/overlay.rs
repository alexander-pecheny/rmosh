//! Speculative changes the client shows before the server has confirmed them.
//!
//! Transliterated from the overlay types in `src/frontend/terminaloverlay.{h,cc}`.
//!
//! A prediction is provisional: it is displayed, then later judged against what the
//! server actually sent. Judging it is the delicate part, because a prediction that
//! merely *looks* right must not earn credit -- credit is what decides whether the
//! client keeps predicting at all.

use mosh_terminal::framebuffer::{Attribute, Cell, Framebuffer};

/// The verdict on a prediction once the server has spoken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// The server has not confirmed this far yet.
    Pending,
    /// Right, and it counts towards trusting predictions.
    Correct,
    /// Right, but too easily right to be evidence of anything.
    CorrectNoCredit,
    Incorrect,
    /// Not a prediction at all.
    Inactive,
}

/// State shared by every kind of prediction.
#[derive(Debug, Clone)]
pub struct ConditionalOverlay {
    /// The local frame number after which this can be judged.
    pub expiration_frame: u64,
    pub col: i32,
    /// Whether this represents a prediction at all.
    pub active: bool,
    /// Epoch until which this is held back from display.
    pub tentative_until_epoch: u64,
    /// When the prediction was made, used to find long-pending ones.
    pub prediction_time: u64,
}

impl ConditionalOverlay {
    pub fn new(expiration_frame: u64, col: i32, tentative_until_epoch: u64) -> Self {
        ConditionalOverlay {
            expiration_frame,
            col,
            active: false,
            tentative_until_epoch,
            prediction_time: u64::MAX,
        }
    }

    /// Is this still too speculative to show?
    pub fn tentative(&self, confirmed_epoch: u64) -> bool {
        self.tentative_until_epoch > confirmed_epoch
    }

    pub fn reset(&mut self) {
        self.expiration_frame = u64::MAX;
        self.tentative_until_epoch = u64::MAX;
        self.active = false;
    }

    pub fn expire(&mut self, expiration_frame: u64, now: u64) {
        self.expiration_frame = expiration_frame;
        self.prediction_time = now;
    }
}

/// A predicted cursor position.
#[derive(Debug, Clone)]
pub struct ConditionalCursorMove {
    pub base: ConditionalOverlay,
    pub row: i32,
}

impl ConditionalCursorMove {
    pub fn new(expiration_frame: u64, row: i32, col: i32, tentative_until_epoch: u64) -> Self {
        ConditionalCursorMove {
            base: ConditionalOverlay::new(expiration_frame, col, tentative_until_epoch),
            row,
        }
    }

    pub fn apply(&self, fb: &mut Framebuffer, confirmed_epoch: u64) {
        if !self.base.active || self.base.tentative(confirmed_epoch) {
            return;
        }
        // The C++ asserts these; a prediction that outlived a resize would otherwise
        // move the cursor off screen, so drop it instead of trusting the assert.
        if self.row >= fb.ds.height() || self.base.col >= fb.ds.width() || fb.ds.origin_mode {
            return;
        }
        fb.ds.move_row(self.row, false);
        fb.ds.move_col(self.base.col, false, false);
    }

    pub fn get_validity(&self, fb: &Framebuffer, late_ack: u64) -> Validity {
        if !self.base.active {
            return Validity::Inactive;
        }
        // A resize can strand a prediction outside the screen.
        if self.row >= fb.ds.height() || self.base.col >= fb.ds.width() {
            return Validity::Incorrect;
        }
        if late_ack >= self.base.expiration_frame {
            if fb.ds.cursor_col() == self.base.col && fb.ds.cursor_row() == self.row {
                return Validity::Correct;
            }
            return Validity::Incorrect;
        }
        Validity::Pending
    }
}

/// A predicted change to one cell.
#[derive(Debug, Clone)]
pub struct ConditionalOverlayCell {
    pub base: ConditionalOverlay,
    pub replacement: Cell,
    /// True when we know the cell changed but not what it became.
    pub unknown: bool,
    /// What was there before. A prediction that merely restores one of these is right
    /// by luck, so it earns no credit.
    pub original_contents: Vec<Cell>,
}

impl ConditionalOverlayCell {
    pub fn new(expiration_frame: u64, col: i32, tentative_until_epoch: u64) -> Self {
        ConditionalOverlayCell {
            base: ConditionalOverlay::new(expiration_frame, col, tentative_until_epoch),
            replacement: Cell::new(0),
            unknown: false,
            original_contents: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.unknown = false;
        self.original_contents.clear();
        self.base.reset();
    }

    /// Reset, but remember what we had predicted so a later identical prediction is
    /// recognised as unearned.
    pub fn reset_with_orig(&mut self) {
        if !self.base.active || self.unknown {
            self.reset();
            return;
        }
        self.original_contents.push(self.replacement.clone());
        self.base.reset();
    }

    pub fn apply(&self, fb: &mut Framebuffer, confirmed_epoch: u64, row: i32, mut flag: bool) {
        if !self.base.active || row >= fb.ds.height() || self.base.col >= fb.ds.width() {
            return;
        }
        if self.base.tentative(confirmed_epoch) {
            return;
        }

        let Some(current) = fb.cell(row, self.base.col) else {
            return;
        };

        // Underlining a blank that is predicted to stay blank would be noise.
        if self.replacement.is_blank() && current.is_blank() {
            flag = false;
        }

        if self.unknown {
            // The last column is left alone: underlining there can push the terminal
            // into a wrap we did not intend.
            if flag && self.base.col != fb.ds.width() - 1 {
                fb.mutable_cell(row, self.base.col)
                    .renditions_mut()
                    .set_attribute(Attribute::Underlined, true);
            }
            return;
        }

        if *current != self.replacement {
            let replacement = self.replacement.clone();
            let cell = fb.mutable_cell(row, self.base.col);
            *cell = replacement;
            if flag {
                cell.renditions_mut()
                    .set_attribute(Attribute::Underlined, true);
            }
        }
    }

    pub fn get_validity(&self, fb: &Framebuffer, row: i32, late_ack: u64) -> Validity {
        if !self.base.active {
            return Validity::Inactive;
        }
        if row >= fb.ds.height() || self.base.col >= fb.ds.width() {
            return Validity::Incorrect;
        }
        let Some(current) = fb.cell(row, self.base.col) else {
            return Validity::Incorrect;
        };

        // Not updated this far yet.
        if late_ack < self.base.expiration_frame {
            return Validity::Pending;
        }

        if self.unknown {
            return Validity::CorrectNoCredit;
        }

        // Predicting a blank is too easy to be right by accident.
        if self.replacement.is_blank() {
            return Validity::CorrectNoCredit;
        }

        if current.contents_match(&self.replacement) {
            // Right, but only because it matches what was already there.
            if self
                .original_contents
                .iter()
                .any(|orig| orig.contents_match(&self.replacement))
            {
                return Validity::CorrectNoCredit;
            }
            return Validity::Correct;
        }
        Validity::Incorrect
    }
}

/// One row's worth of predicted cells.
#[derive(Debug, Clone)]
pub struct ConditionalOverlayRow {
    pub row_num: i32,
    pub overlay_cells: Vec<ConditionalOverlayCell>,
}

impl ConditionalOverlayRow {
    pub fn new(row_num: i32, num_cols: i32) -> Self {
        ConditionalOverlayRow {
            row_num,
            overlay_cells: (0..num_cols)
                .map(|col| ConditionalOverlayCell::new(0, col, 0))
                .collect(),
        }
    }

    pub fn apply(&self, fb: &mut Framebuffer, confirmed_epoch: u64, flag: bool) {
        for cell in &self.overlay_cells {
            cell.apply(fb, confirmed_epoch, self.row_num, flag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_with(text: &str) -> Cell {
        let mut c = Cell::new(0);
        for ch in text.chars() {
            c.append(ch);
        }
        c
    }

    fn overlay_cell(col: i32, text: &str, expiration: u64) -> ConditionalOverlayCell {
        let mut o = ConditionalOverlayCell::new(expiration, col, 0);
        o.base.active = true;
        o.replacement = cell_with(text);
        o
    }

    #[test]
    fn an_inactive_prediction_is_not_judged() {
        let fb = Framebuffer::new(10, 3);
        let o = ConditionalOverlayCell::new(0, 0, 0);
        assert_eq!(o.get_validity(&fb, 0, 100), Validity::Inactive);
    }

    #[test]
    fn a_prediction_the_server_has_not_reached_is_pending() {
        let fb = Framebuffer::new(10, 3);
        let o = overlay_cell(0, "x", 50);
        // The server has only confirmed up to frame 10, so we cannot know yet.
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::Pending);
    }

    #[test]
    fn a_matching_prediction_is_correct() {
        let mut fb = Framebuffer::new(10, 3);
        fb.mutable_cell(0, 0).append('x');
        let o = overlay_cell(0, "x", 5);
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::Correct);
    }

    #[test]
    fn a_mismatching_prediction_is_incorrect() {
        let mut fb = Framebuffer::new(10, 3);
        fb.mutable_cell(0, 0).append('y');
        let o = overlay_cell(0, "x", 5);
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::Incorrect);
    }

    #[test]
    fn predicting_a_blank_earns_no_credit() {
        let fb = Framebuffer::new(10, 3);
        let mut o = ConditionalOverlayCell::new(5, 0, 0);
        o.base.active = true;
        // Blank replacement: right far too easily to be evidence of anything.
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::CorrectNoCredit);
    }

    #[test]
    fn an_unknown_prediction_earns_no_credit() {
        let mut fb = Framebuffer::new(10, 3);
        fb.mutable_cell(0, 0).append('x');
        let mut o = overlay_cell(0, "x", 5);
        o.unknown = true;
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::CorrectNoCredit);
    }

    #[test]
    fn restoring_what_was_already_there_earns_no_credit() {
        let mut fb = Framebuffer::new(10, 3);
        fb.mutable_cell(0, 0).append('x');

        let mut o = overlay_cell(0, "x", 5);
        // We previously predicted this same content here, so being right now says
        // nothing about whether we are predicting well.
        o.original_contents.push(cell_with("x"));
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::CorrectNoCredit);

        // With a different history, the same prediction does earn credit.
        let mut o = overlay_cell(0, "x", 5);
        o.original_contents.push(cell_with("q"));
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::Correct);
    }

    #[test]
    fn a_prediction_stranded_by_a_resize_is_incorrect() {
        let fb = Framebuffer::new(4, 2);
        // Predicted at a column that no longer exists.
        let o = overlay_cell(9, "x", 5);
        assert_eq!(o.get_validity(&fb, 0, 10), Validity::Incorrect);
        // And at a row that no longer exists.
        let o = overlay_cell(0, "x", 5);
        assert_eq!(o.get_validity(&fb, 9, 10), Validity::Incorrect);
    }

    #[test]
    fn applying_a_prediction_changes_the_cell() {
        let mut fb = Framebuffer::new(10, 3);
        let o = overlay_cell(2, "z", 5);
        o.apply(&mut fb, 0, 0, false);
        assert_eq!(fb.cell(0, 2).unwrap().contents(), "z");
    }

    #[test]
    fn a_tentative_prediction_is_not_shown() {
        let mut fb = Framebuffer::new(10, 3);
        let mut o = overlay_cell(2, "z", 5);
        // Held back until a later epoch than the one confirmed.
        o.base.tentative_until_epoch = 7;
        o.apply(&mut fb, 3, 0, false);
        assert!(fb.cell(0, 2).unwrap().is_empty());

        // Once the epoch is confirmed it appears.
        o.apply(&mut fb, 7, 0, false);
        assert_eq!(fb.cell(0, 2).unwrap().contents(), "z");
    }

    #[test]
    fn flagging_underlines_a_prediction() {
        let mut fb = Framebuffer::new(10, 3);
        let o = overlay_cell(2, "z", 5);
        o.apply(&mut fb, 0, 0, true);
        assert!(fb
            .cell(0, 2)
            .unwrap()
            .renditions()
            .attribute(Attribute::Underlined));
    }

    #[test]
    fn a_blank_predicted_over_a_blank_is_not_underlined() {
        let mut fb = Framebuffer::new(10, 3);
        let mut o = ConditionalOverlayCell::new(5, 2, 0);
        o.base.active = true;
        // Underlining a blank that stays blank would be visible noise for no reason.
        o.apply(&mut fb, 0, 0, true);
        assert!(!fb
            .cell(0, 2)
            .unwrap()
            .renditions()
            .attribute(Attribute::Underlined));
    }

    #[test]
    fn an_unknown_prediction_underlines_but_does_not_overwrite() {
        let mut fb = Framebuffer::new(10, 3);
        fb.mutable_cell(0, 2).append('q');
        let mut o = overlay_cell(2, "z", 5);
        o.unknown = true;
        o.apply(&mut fb, 0, 0, true);
        // We know something changed but not what, so the content is left alone.
        assert_eq!(fb.cell(0, 2).unwrap().contents(), "q");
        assert!(fb
            .cell(0, 2)
            .unwrap()
            .renditions()
            .attribute(Attribute::Underlined));
    }

    #[test]
    fn the_last_column_is_never_underlined_for_an_unknown() {
        let mut fb = Framebuffer::new(10, 3);
        let mut o = overlay_cell(9, "z", 5);
        o.unknown = true;
        o.apply(&mut fb, 0, 0, true);
        // Underlining the final column can provoke a wrap we did not intend.
        assert!(!fb
            .cell(0, 9)
            .unwrap()
            .renditions()
            .attribute(Attribute::Underlined));
    }

    #[test]
    fn reset_with_orig_remembers_what_was_predicted() {
        let mut o = overlay_cell(0, "x", 5);
        o.reset_with_orig();
        assert!(!o.base.active);
        assert_eq!(o.original_contents.len(), 1);

        // An unknown prediction has nothing worth remembering.
        let mut o = overlay_cell(0, "x", 5);
        o.unknown = true;
        o.reset_with_orig();
        assert!(o.original_contents.is_empty());
    }

    #[test]
    fn a_cursor_prediction_is_judged_on_position() {
        let mut fb = Framebuffer::new(10, 5);
        fb.ds.move_row(2, false);
        fb.ds.move_col(3, false, false);

        let mut c = ConditionalCursorMove::new(5, 2, 3, 0);
        c.base.active = true;
        assert_eq!(c.get_validity(&fb, 10), Validity::Correct);

        let mut c = ConditionalCursorMove::new(5, 1, 3, 0);
        c.base.active = true;
        assert_eq!(c.get_validity(&fb, 10), Validity::Incorrect);

        // Not confirmed that far yet.
        let mut c = ConditionalCursorMove::new(50, 2, 3, 0);
        c.base.active = true;
        assert_eq!(c.get_validity(&fb, 10), Validity::Pending);
    }

    #[test]
    fn a_cursor_prediction_moves_the_cursor() {
        let mut fb = Framebuffer::new(10, 5);
        let mut c = ConditionalCursorMove::new(5, 2, 3, 0);
        c.base.active = true;
        c.apply(&mut fb, 0);
        assert_eq!((fb.ds.cursor_row(), fb.ds.cursor_col()), (2, 3));
    }

    #[test]
    fn a_cursor_prediction_outside_the_screen_is_dropped() {
        // The C++ asserts here. A prediction that outlived a resize would move the
        // cursor off screen, so it is discarded instead.
        let mut fb = Framebuffer::new(4, 2);
        let mut c = ConditionalCursorMove::new(5, 9, 9, 0);
        c.base.active = true;
        c.apply(&mut fb, 0);
        assert_eq!((fb.ds.cursor_row(), fb.ds.cursor_col()), (0, 0));
    }

    #[test]
    fn a_row_applies_every_cell_it_holds() {
        let mut fb = Framebuffer::new(5, 2);
        let mut row = ConditionalOverlayRow::new(0, 5);
        for (i, ch) in "abc".chars().enumerate() {
            row.overlay_cells[i].base.active = true;
            row.overlay_cells[i].replacement = cell_with(&ch.to_string());
        }
        row.apply(&mut fb, 0, false);
        let mut line = String::new();
        for x in 0..3 {
            fb.cell(0, x).unwrap().print_grapheme(&mut line);
        }
        assert_eq!(line, "abc");
    }
}
