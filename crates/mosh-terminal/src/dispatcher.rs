//! Terminal functions: the routines a CSI, escape or control character activates.
//!
//! Transliterated from `terminaldispatcher.{h,cc}` and `terminalfunctions.cc`.
//!
//! The C++ builds a global map at static-initialisation time, one entry per function,
//! keyed by the dispatch characters. Here the same table is a `match`, which removes the
//! global and the static-initialisation-order dance without changing which key selects
//! which routine.

use crate::framebuffer::{
    Framebuffer, Hyperlink, MouseEncodingMode, MouseReportingMode, Renditions,
};

/// Which table a dispatch key is looked up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionType {
    Escape,
    Csi,
    Control,
}

/// Guards against an escape sequence driving a very long loop.
const PARAM_MAX: i32 = 65535;

/// Enough for 16 five-character parameters plus 15 semicolons.
const MAX_PARAM_BYTES: usize = 100;

/// Never needs more than two, but leave room.
const MAX_DISPATCH_CHARS: usize = 8;

const MAXIMUM_CLIPBOARD_SIZE: usize = 16 * 1024;

/// Accumulates the parameters and intermediate characters of one escape sequence, then
/// runs the function they select.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dispatcher {
    params: String,
    parsed_params: Vec<i32>,
    parsed: bool,

    dispatch_chars: String,
    osc_string: Vec<char>,

    /// The reply string, sent back to the application.
    pub terminal_to_host: String,
}

impl Dispatcher {
    pub fn new() -> Self {
        Dispatcher::default()
    }

    pub fn new_param_char(&mut self, ch: char) {
        debug_assert!(ch == ';' || ch.is_ascii_digit());
        if self.params.len() < MAX_PARAM_BYTES {
            self.params.push(ch);
        }
        self.parsed = false;
    }

    pub fn collect(&mut self, ch: char) {
        if self.dispatch_chars.len() < MAX_DISPATCH_CHARS && (ch as u32) <= 255 {
            self.dispatch_chars.push(ch);
        }
    }

    pub fn clear(&mut self) {
        self.params.clear();
        self.dispatch_chars.clear();
        self.parsed = false;
    }

    pub fn dispatch_chars(&self) -> &str {
        &self.dispatch_chars
    }

    pub fn osc_string(&self) -> &[char] {
        &self.osc_string
    }

    /// Split the parameter string on semicolons.
    ///
    /// Follows `strtol` semantics as the C++ does: a segment with no digits, or one that
    /// overflows, becomes -1, which `getparam` then turns into the caller's default.
    fn parse_params(&mut self) {
        if self.parsed {
            return;
        }
        self.parsed_params.clear();

        for segment in self.params.split(';') {
            let digits: String = segment.chars().take_while(|c| c.is_ascii_digit()).collect();
            let val = match digits.parse::<i64>() {
                Ok(v) if v <= PARAM_MAX as i64 => v as i32,
                // No digits at all, or a value beyond what we will act on.
                _ => -1,
            };
            self.parsed_params.push(val);
        }

        self.parsed = true;
    }

    /// Parameter `n`, or `default` when it is absent, empty or zero.
    pub fn getparam(&mut self, n: usize, default: i32) -> i32 {
        self.parse_params();
        let mut ret = *self.parsed_params.get(n).unwrap_or(&default);
        if ret < 1 {
            ret = default;
        }
        ret
    }

    pub fn param_count(&mut self) -> usize {
        self.parse_params();
        self.parsed_params.len()
    }

    /// Debug rendering, mirroring `Dispatcher::str`.
    pub fn str(&self) -> String {
        format!(
            "[dispatch=\"{}\" params=\"{}\"]",
            self.dispatch_chars, self.params
        )
    }

    pub fn osc_put(&mut self, ch: char) {
        if self.osc_string.len() < MAXIMUM_CLIPBOARD_SIZE {
            self.osc_string.push(ch);
        }
    }

    pub fn osc_start(&mut self) {
        self.osc_string.clear();
    }

    /// Run the function selected by the accumulated dispatch characters.
    pub fn dispatch(&mut self, ty: FunctionType, ch: char, fb: &mut Framebuffer) {
        // The final character is part of the key.
        if ty == FunctionType::Escape || ty == FunctionType::Csi {
            self.collect(ch);
        }

        let key = if ty == FunctionType::Control {
            let mut s = String::new();
            s.push(ch);
            s
        } else {
            self.dispatch_chars.clone()
        };

        match lookup(ty, &key) {
            None => {
                // An unknown function still clears the wrap state.
                fb.ds.next_print_will_wrap = false;
            }
            Some(entry) => {
                if entry.clears_wrap_state {
                    fb.ds.next_print_will_wrap = false;
                }
                (entry.function)(fb, self);
            }
        }
    }
}

/// One entry of the dispatch table.
struct Entry {
    function: fn(&mut Framebuffer, &mut Dispatcher),
    /// Most functions cancel a pending wrap; a few deliberately preserve it.
    clears_wrap_state: bool,
}

fn entry(function: fn(&mut Framebuffer, &mut Dispatcher)) -> Option<Entry> {
    Some(Entry {
        function,
        clears_wrap_state: true,
    })
}

fn entry_keeping_wrap(function: fn(&mut Framebuffer, &mut Dispatcher)) -> Option<Entry> {
    Some(Entry {
        function,
        clears_wrap_state: false,
    })
}

fn lookup(ty: FunctionType, key: &str) -> Option<Entry> {
    match ty {
        FunctionType::Csi => match key {
            "K" => entry(csi_el),
            "J" => entry(csi_ed),
            "A" | "B" | "C" | "D" | "H" | "f" => entry(csi_cursormove),
            "c" => entry(csi_da),
            ">c" => entry(csi_sda),
            "I" | "Z" => entry_keeping_wrap(csi_cxt),
            "g" => entry_keeping_wrap(csi_tbc),
            "?h" => entry_keeping_wrap(csi_decsm),
            "?l" => entry_keeping_wrap(csi_decrm),
            "h" => entry(csi_sm),
            "l" => entry(csi_rm),
            "r" => entry(csi_decstbm),
            // Changing renditions does not clear the wrap flag.
            "m" => entry_keeping_wrap(csi_sgr),
            "n" => entry(csi_dsr),
            "L" => entry(csi_il),
            "M" => entry(csi_dl),
            "@" => entry(csi_ich),
            "P" => entry(csi_dch),
            "d" => entry(csi_vpa),
            "G" | "\x60" => entry(csi_hpa),
            "X" => entry(csi_ech),
            "!p" => entry(csi_decstr),
            "S" => entry(csi_sd),
            "T" => entry(csi_su),
            _ => None,
        },
        FunctionType::Escape => match key {
            "#8" => entry(esc_decaln),
            "7" => entry(esc_decsc),
            "8" => entry(esc_decrc),
            "c" => entry(esc_ris),
            _ => None,
        },
        FunctionType::Control => match key {
            // Index, vertical tab and form feed all behave as a line feed.
            "\x0a" | "\u{84}" | "\x0b" | "\x0c" => entry(ctrl_lf),
            "\x0d" => entry(ctrl_cr),
            "\x08" => entry(ctrl_bs),
            "\u{8d}" => entry(ctrl_ri),
            "\u{85}" => entry(ctrl_nel),
            "\x09" => entry_keeping_wrap(ctrl_ht),
            "\u{88}" => entry(ctrl_hts),
            "\x07" => entry(ctrl_bel),
            _ => None,
        },
    }
}

fn clearline(fb: &mut Framebuffer, row: i32, start: i32, end: i32) {
    for col in start..=end {
        fb.reset_cell(row, col);
    }
}

/// Erase in line.
fn csi_el(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    match dispatch.getparam(0, 0) {
        0 => clearline(fb, -1, fb.ds.cursor_col(), fb.ds.width() - 1),
        1 => clearline(fb, -1, 0, fb.ds.cursor_col()),
        2 => fb.reset_row(-1),
        _ => {}
    }
}

/// Erase in display.
fn csi_ed(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    match dispatch.getparam(0, 0) {
        0 => {
            clearline(fb, -1, fb.ds.cursor_col(), fb.ds.width() - 1);
            for y in (fb.ds.cursor_row() + 1)..fb.ds.height() {
                fb.reset_row(y);
            }
        }
        1 => {
            for y in 0..fb.ds.cursor_row() {
                fb.reset_row(y);
            }
            clearline(fb, -1, 0, fb.ds.cursor_col());
        }
        2 => {
            for y in 0..fb.ds.height() {
                fb.reset_row(y);
            }
        }
        _ => {}
    }
}

/// Cursor movement, relative and absolute.
fn csi_cursormove(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let num = dispatch.getparam(0, 1);
    match dispatch.dispatch_chars().chars().next() {
        Some('A') => fb.ds.move_row(-num, true),
        Some('B') => fb.ds.move_row(num, true),
        Some('C') => fb.ds.move_col(num, true, false),
        Some('D') => fb.ds.move_col(-num, true, false),
        Some('H') | Some('f') => {
            let row = dispatch.getparam(0, 1);
            let col = dispatch.getparam(1, 1);
            fb.ds.move_row(row - 1, false);
            fb.ds.move_col(col - 1, false, false);
        }
        _ => {}
    }
}

/// Device attributes: identify as a plain vt220.
fn csi_da(_fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    dispatch.terminal_to_host.push_str("\x1b[?62c");
}

/// Secondary device attributes.
fn csi_sda(_fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    dispatch.terminal_to_host.push_str("\x1b[>1;10;0c");
}

/// Screen alignment diagnostic: fill the screen with 'E'.
fn esc_decaln(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    for y in 0..fb.ds.height() {
        for x in 0..fb.ds.width() {
            fb.reset_cell(y, x);
            fb.mutable_cell(y, x).append('E');
        }
    }
}

fn ctrl_lf(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.move_rows_autoscroll(1);
}

fn ctrl_cr(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.move_col(0, false, false);
}

fn ctrl_bs(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.move_col(-1, true, false);
}

/// Reverse index: a backwards line feed.
fn ctrl_ri(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.move_rows_autoscroll(-1);
}

fn ctrl_nel(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.move_col(0, false, false);
    fb.move_rows_autoscroll(1);
}

fn ht_n(fb: &mut Framebuffer, count: i32) {
    let mut col = fb.ds.next_tab(count);
    if col == -1 {
        // No tab stop ahead; go to the end of the line.
        col = fb.ds.width() - 1;
    }

    // A horizontal tab is the only operation that preserves but does not set the wrap
    // state. It also starts a new grapheme.
    let wrap_state_save = fb.ds.next_print_will_wrap;
    fb.ds.move_col(col, false, false);
    fb.ds.next_print_will_wrap = wrap_state_save;
}

fn ctrl_ht(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    ht_n(fb, 1);
}

fn csi_cxt(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let mut param = dispatch.getparam(0, 1);
    if dispatch.dispatch_chars().starts_with('Z') {
        param = -param;
    }
    if param == 0 {
        return;
    }
    ht_n(fb, param);
}

fn ctrl_hts(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.set_tab();
}

/// Tabulation clear.
fn csi_tbc(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    match dispatch.getparam(0, 0) {
        0 => {
            let col = fb.ds.cursor_col();
            fb.ds.clear_tab(col);
        }
        3 => {
            fb.ds.clear_default_tabs();
            for x in 0..fb.ds.width() {
                fb.ds.clear_tab(x);
            }
        }
        _ => {}
    }
}

/// Which private mode a parameter names, if any.
///
/// Some parameters have a side effect even though they set no flag, which is why this
/// takes the framebuffer.
fn set_dec_mode(fb: &mut Framebuffer, param: i32, value: bool) {
    match param {
        1 => fb.ds.application_mode_cursor_keys = value,
        3 => {
            // 80/132 column switch. Ignored, but it clears the screen.
            fb.ds.move_row(0, false);
            fb.ds.move_col(0, false, false);
            for y in 0..fb.ds.height() {
                fb.reset_row(y);
            }
        }
        5 => fb.ds.reverse_video = value,
        6 => {
            fb.ds.move_row(0, false);
            fb.ds.move_col(0, false, false);
            fb.ds.origin_mode = value;
        }
        7 => fb.ds.auto_wrap_mode = value,
        25 => fb.ds.cursor_visible = value,
        1004 => fb.ds.mouse_focus_event = value,
        1007 => fb.ds.mouse_alternate_scroll = value,
        2004 => fb.ds.bracketed_paste = value,
        _ => {}
    }
}

/// Set private mode.
fn csi_decsm(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    for i in 0..dispatch.param_count() {
        let param = dispatch.getparam(i, 0);
        if param == 9 || (1000..=1003).contains(&param) {
            fb.ds.mouse_reporting_mode = match param {
                9 => MouseReportingMode::X10,
                1000 => MouseReportingMode::Vt220,
                1001 => MouseReportingMode::Vt220Hilight,
                1002 => MouseReportingMode::BtnEvent,
                _ => MouseReportingMode::AnyEvent,
            };
        } else if param == 1005 || param == 1006 || param == 1015 {
            fb.ds.mouse_encoding_mode = match param {
                1005 => MouseEncodingMode::Utf8,
                1006 => MouseEncodingMode::Sgr,
                _ => MouseEncodingMode::Urxvt,
            };
        } else {
            set_dec_mode(fb, param, true);
        }
    }
}

/// Clear private mode.
fn csi_decrm(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    for i in 0..dispatch.param_count() {
        let param = dispatch.getparam(i, 0);
        if param == 9 || (1000..=1003).contains(&param) {
            fb.ds.mouse_reporting_mode = MouseReportingMode::None;
        } else if param == 1005 || param == 1006 || param == 1015 {
            fb.ds.mouse_encoding_mode = MouseEncodingMode::Default;
        } else {
            set_dec_mode(fb, param, false);
        }
    }
}

/// Set mode. Only insert/replace is implemented.
fn csi_sm(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    for i in 0..dispatch.param_count() {
        if dispatch.getparam(i, 0) == 4 {
            fb.ds.insert_mode = true;
        }
    }
}

/// Clear mode.
fn csi_rm(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    for i in 0..dispatch.param_count() {
        if dispatch.getparam(i, 0) == 4 {
            fb.ds.insert_mode = false;
        }
    }
}

/// Set top and bottom margins.
fn csi_decstbm(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let top = dispatch.getparam(0, 1);
    let bottom = dispatch.getparam(1, fb.ds.height());

    if bottom <= top || top > fb.ds.height() || (top == 0 && bottom == 1) {
        return; // invalid; xterm ignores it
    }

    fb.ds.set_scrolling_region(top - 1, bottom - 1);
    fb.ds.move_row(0, false);
    fb.ds.move_col(0, false, false);
}

fn ctrl_bel(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ring_bell();
}

/// Select graphics rendition: bold, blinking, colours and so on.
fn csi_sgr(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let mut i = 0;
    while i < dispatch.param_count() {
        let rendition = dispatch.getparam(i, 0);

        // 38;5;Ps and 48;5;Ps need special handling: a Ps of 0 selects colour 0 here,
        // where elsewhere a 0 rendition means reset to default.
        if (rendition == 38 || rendition == 48)
            && dispatch.param_count() - i >= 3
            && dispatch.getparam(i + 1, -1) == 5
        {
            let color = dispatch.getparam(i + 2, 0);
            if rendition == 38 {
                fb.ds.set_foreground_color(color);
            } else {
                fb.ds.set_background_color(color);
            }
            i += 3;
            continue;
        }

        // True colour: 38;2;r;g;b and 48;2;r;g;b.
        if (rendition == 38 || rendition == 48)
            && dispatch.param_count() - i >= 5
            && dispatch.getparam(i + 1, -1) == 2
        {
            let red = dispatch.getparam(i + 2, 0) as u32;
            let green = dispatch.getparam(i + 3, 0) as u32;
            let blue = dispatch.getparam(i + 4, 0) as u32;
            let color = Renditions::make_true_color(red, green, blue) as i32;
            if rendition == 38 {
                fb.ds.set_foreground_color(color);
            } else {
                fb.ds.set_background_color(color);
            }
            i += 5;
            continue;
        }

        fb.ds.add_rendition(rendition as u32);
        i += 1;
    }
}

fn esc_decsc(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.save_cursor();
}

fn esc_decrc(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.ds.restore_cursor();
}

/// Device status report, including the cursor position query used by resize.
fn csi_dsr(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    match dispatch.getparam(0, 0) {
        5 => dispatch.terminal_to_host.push_str("\x1b[0n"),
        6 => {
            let reply = format!(
                "\x1b[{};{}R",
                fb.ds.cursor_row() + 1,
                fb.ds.cursor_col() + 1
            );
            dispatch.terminal_to_host.push_str(&reply);
        }
        _ => {}
    }
}

fn csi_il(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let lines = dispatch.getparam(0, 1);
    fb.insert_line(fb.ds.cursor_row(), lines);
    // The vt220 manual and ECMA-48 say to move to the first column, but xterm and
    // gnome-terminal do not, and we follow them.
    fb.ds.move_col(0, false, false);
}

fn csi_dl(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let lines = dispatch.getparam(0, 1);
    fb.delete_line(fb.ds.cursor_row(), lines);
    fb.ds.move_col(0, false, false);
}

fn csi_ich(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let cells = dispatch.getparam(0, 1);
    for _ in 0..cells {
        fb.insert_cell(fb.ds.cursor_row(), fb.ds.cursor_col());
    }
}

fn csi_dch(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let cells = dispatch.getparam(0, 1);
    for _ in 0..cells {
        fb.delete_cell(fb.ds.cursor_row(), fb.ds.cursor_col());
    }
}

/// Line position absolute.
fn csi_vpa(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let row = dispatch.getparam(0, 1);
    fb.ds.move_row(row - 1, false);
}

/// Character position absolute.
fn csi_hpa(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let col = dispatch.getparam(0, 1);
    fb.ds.move_col(col - 1, false, false);
}

/// Erase character.
fn csi_ech(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let num = dispatch.getparam(0, 1);
    let mut limit = fb.ds.cursor_col() + num - 1;
    if limit >= fb.ds.width() {
        limit = fb.ds.width() - 1;
    }
    clearline(fb, -1, fb.ds.cursor_col(), limit);
}

/// Reset to initial state.
fn esc_ris(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.reset();
}

fn csi_decstr(fb: &mut Framebuffer, _dispatch: &mut Dispatcher) {
    fb.soft_reset();
}

/// Scroll down, terminfo `indn`.
fn csi_sd(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let n = dispatch.getparam(0, 1);
    fb.scroll(n);
}

/// Scroll up, terminfo `rin`.
fn csi_su(fb: &mut Framebuffer, dispatch: &mut Dispatcher) {
    let n = dispatch.getparam(0, 1);
    fb.scroll(-n);
}

/// Render an OSC payload as printable ASCII, rejecting anything outside 32..=126.
fn parse_printable_osc(osc: &[char]) -> Option<String> {
    let mut out = String::with_capacity(osc.len());
    for &c in osc {
        if (c as u32) < 32 || (c as u32) > 126 {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// OSC 8 hyperlink: `ESC ] 8 ; params ; url BEL`.
fn osc_8(osc: &str, fb: &mut Framebuffer) {
    if osc.len() <= 2 || osc.as_bytes()[1] != b';' {
        return; // malformed
    }
    let Some(second_semicolon) = osc[2..].find(';').map(|i| i + 2) else {
        return; // missing the second semicolon
    };
    let params = osc[2..second_semicolon].to_string();
    let url = osc[second_semicolon + 1..].to_string();
    fb.ds.set_hyperlink(Hyperlink::new(params, url));
}

/// A palette query (`OSC 4;Ps;?`) or a dynamic colour query (`OSC 10..19;?`).
///
/// mosh has no colours of its own to report, so the query is forwarded to the client's
/// terminal and the reply reaches the application as ordinary input.
fn parse_color_query(osc: &[char]) -> Option<String> {
    if osc.last() != Some(&'?') {
        return None;
    }
    let body = parse_printable_osc(osc)?;
    let separator = body.find(';')?;
    let command = &body[..separator];

    let dynamic_color =
        command.len() == 2 && command.starts_with('1') && command.as_bytes()[1].is_ascii_digit();
    if command != "4" && !dynamic_color {
        return None;
    }

    Some(format!("\x1b]{body}\x1b\\"))
}

impl Dispatcher {
    /// Act on a completed Operating System Command.
    pub fn osc_dispatch(&mut self, fb: &mut Framebuffer) {
        let osc = &self.osc_string;

        // OSC 52;Pc;Pd -- clipboard. Pd is base64 data, or "?" to query.
        if osc.len() >= 4
            && osc[0] == '5'
            && osc[1] == '2'
            && osc[2] == ';'
            && osc[3..].contains(&';')
        {
            let clipboard: Vec<char> = osc[3..].to_vec();
            fb.set_clipboard(clipboard);
            return;
        }

        if let Some(query) = parse_color_query(osc) {
            fb.add_color_query(&query);
            return;
        }

        if osc.is_empty() {
            return;
        }

        let mut cmd_num: i64 = -1;
        let mut offset = 0usize;
        if osc[0] == ';' {
            // `ESC ] ; title BEL` -- treat the missing number as zero.
            cmd_num = 0;
            offset = 1;
        } else if osc.len() >= 2 && osc[1] == ';' {
            // `ESC ] X ; title BEL`, where X is 0 (icon and title), 1 (icon) or 2 (title).
            cmd_num = osc[0] as i64 - '0' as i64;
            offset = 2;
        }

        if cmd_num == 8 {
            let Some(osc_8_str) = parse_printable_osc(osc) else {
                return;
            };
            osc_8(&osc_8_str, fb);
            return;
        }

        let set_icon = cmd_num == 0 || cmd_num == 1;
        let set_title = cmd_num == 0 || cmd_num == 2;
        if set_icon || set_title {
            fb.set_title_initialized();
            let title_length = osc.len().min(256);
            // A title shorter than the offset yields an empty title rather than a panic.
            let newtitle: Vec<char> = if offset < title_length {
                osc[offset..title_length].to_vec()
            } else {
                Vec::new()
            };
            if set_icon {
                fb.set_icon_name(newtitle.clone());
            }
            if set_title {
                fb.set_window_title(newtitle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::Attribute;

    fn dispatch_csi(fb: &mut Framebuffer, params: &str, final_ch: char) -> Dispatcher {
        let mut d = Dispatcher::new();
        for c in params.chars() {
            if c.is_ascii_digit() || c == ';' {
                d.new_param_char(c);
            } else {
                d.collect(c);
            }
        }
        d.dispatch(FunctionType::Csi, final_ch, fb);
        d
    }

    #[test]
    fn absent_parameters_fall_back_to_the_default() {
        let mut d = Dispatcher::new();
        assert_eq!(d.getparam(0, 7), 7);
        // An empty parameter string still yields one (empty) parameter.
        assert_eq!(d.param_count(), 1);
    }

    #[test]
    fn zero_and_empty_parameters_use_the_default() {
        let mut d = Dispatcher::new();
        for c in "0;;5".chars() {
            d.new_param_char(c);
        }
        assert_eq!(d.param_count(), 3);
        assert_eq!(d.getparam(0, 9), 9); // 0 is below 1, so the default wins
        assert_eq!(d.getparam(1, 9), 9); // empty
        assert_eq!(d.getparam(2, 9), 5);
    }

    #[test]
    fn oversized_parameters_are_clamped_to_the_default() {
        let mut d = Dispatcher::new();
        for c in "999999".chars() {
            d.new_param_char(c);
        }
        // Beyond PARAM_MAX, so it must not drive a long loop.
        assert_eq!(d.getparam(0, 1), 1);
    }

    #[test]
    fn the_parameter_string_is_bounded() {
        let mut d = Dispatcher::new();
        for _ in 0..500 {
            d.new_param_char('1');
        }
        assert!(d.params.len() <= MAX_PARAM_BYTES);
    }

    #[test]
    fn cursor_movement_is_relative_or_absolute() {
        let mut fb = Framebuffer::new(20, 10);
        dispatch_csi(&mut fb, "5", 'B');
        assert_eq!(fb.ds.cursor_row(), 5);
        dispatch_csi(&mut fb, "2", 'A');
        assert_eq!(fb.ds.cursor_row(), 3);
        dispatch_csi(&mut fb, "4", 'C');
        assert_eq!(fb.ds.cursor_col(), 4);

        dispatch_csi(&mut fb, "1;1", 'H');
        assert_eq!((fb.ds.cursor_row(), fb.ds.cursor_col()), (0, 0));
    }

    #[test]
    fn erase_in_line_honours_its_three_modes() {
        let mut fb = Framebuffer::new(5, 1);
        let fill = |fb: &mut Framebuffer| {
            for c in 0..5 {
                fb.reset_cell(0, c);
                fb.mutable_cell(0, c).append('x');
            }
        };

        fill(&mut fb);
        fb.ds.move_col(2, false, false);
        dispatch_csi(&mut fb, "0", 'K');
        assert_eq!(fb.cell(0, 1).unwrap().contents(), "x");
        assert!(fb.cell(0, 2).unwrap().is_empty());
        assert!(fb.cell(0, 4).unwrap().is_empty());

        fill(&mut fb);
        fb.ds.move_col(2, false, false);
        dispatch_csi(&mut fb, "1", 'K');
        assert!(fb.cell(0, 0).unwrap().is_empty());
        assert!(fb.cell(0, 2).unwrap().is_empty());
        assert_eq!(fb.cell(0, 3).unwrap().contents(), "x");

        fill(&mut fb);
        dispatch_csi(&mut fb, "2", 'K');
        for c in 0..5 {
            assert!(fb.cell(0, c).unwrap().is_empty());
        }
    }

    #[test]
    fn sgr_applies_attributes_and_colours() {
        let mut fb = Framebuffer::new(10, 2);
        dispatch_csi(&mut fb, "1", 'm');
        assert!(fb.ds.renditions().attribute(Attribute::Bold));

        dispatch_csi(&mut fb, "0", 'm');
        assert!(!fb.ds.renditions().attribute(Attribute::Bold));
    }

    #[test]
    fn sgr_256_colour_treats_zero_as_a_colour_not_a_reset() {
        let mut fb = Framebuffer::new(10, 2);
        dispatch_csi(&mut fb, "1;38;5;0", 'm');
        // Bold must survive: the 0 belongs to the colour, not to a reset.
        assert!(fb.ds.renditions().attribute(Attribute::Bold));
        assert_eq!(fb.ds.renditions().sgr(), "\x1b[0;1;30m");
    }

    #[test]
    fn sgr_true_colour_consumes_five_parameters() {
        let mut fb = Framebuffer::new(10, 2);
        dispatch_csi(&mut fb, "38;2;10;20;30;1", 'm');
        assert_eq!(fb.ds.renditions().sgr(), "\x1b[0;1;38;2;10;20;30m");
    }

    #[test]
    fn unknown_functions_clear_the_wrap_state() {
        let mut fb = Framebuffer::new(10, 2);
        fb.ds.next_print_will_wrap = true;
        dispatch_csi(&mut fb, "", 'Q'); // not a function we implement
        assert!(!fb.ds.next_print_will_wrap);
    }

    #[test]
    fn sgr_and_tab_preserve_the_wrap_state() {
        let mut fb = Framebuffer::new(10, 2);
        fb.ds.next_print_will_wrap = true;
        dispatch_csi(&mut fb, "1", 'm');
        assert!(fb.ds.next_print_will_wrap, "SGR cleared the wrap state");

        fb.ds.next_print_will_wrap = true;
        let mut d = Dispatcher::new();
        d.dispatch(FunctionType::Control, '\t', &mut fb);
        assert!(fb.ds.next_print_will_wrap, "HT cleared the wrap state");
    }

    #[test]
    fn device_attributes_reply_as_a_vt220() {
        let mut fb = Framebuffer::new(10, 2);
        let d = dispatch_csi(&mut fb, "", 'c');
        assert_eq!(d.terminal_to_host, "\x1b[?62c");
    }

    #[test]
    fn cursor_position_report_is_one_based() {
        let mut fb = Framebuffer::new(10, 5);
        fb.ds.move_row(2, false);
        fb.ds.move_col(3, false, false);
        let d = dispatch_csi(&mut fb, "6", 'n');
        assert_eq!(d.terminal_to_host, "\x1b[3;4R");
    }

    #[test]
    fn private_modes_set_and_clear() {
        let mut fb = Framebuffer::new(10, 5);
        let mut d = Dispatcher::new();
        d.collect('?');
        d.new_param_char('2');
        d.new_param_char('5');
        d.dispatch(FunctionType::Csi, 'l', &mut fb);
        assert!(!fb.ds.cursor_visible);

        let mut d = Dispatcher::new();
        d.collect('?');
        d.new_param_char('2');
        d.new_param_char('5');
        d.dispatch(FunctionType::Csi, 'h', &mut fb);
        assert!(fb.ds.cursor_visible);
    }

    #[test]
    fn scrolling_region_rejects_invalid_ranges() {
        let mut fb = Framebuffer::new(10, 10);
        dispatch_csi(&mut fb, "5;2", 'r'); // bottom above top
        assert_eq!(fb.ds.scrolling_region_top_row(), 0);
        assert_eq!(fb.ds.scrolling_region_bottom_row(), 9);

        dispatch_csi(&mut fb, "2;5", 'r');
        assert_eq!(fb.ds.scrolling_region_top_row(), 1);
        assert_eq!(fb.ds.scrolling_region_bottom_row(), 4);
    }

    #[test]
    fn decaln_fills_the_screen() {
        let mut fb = Framebuffer::new(4, 3);
        let mut d = Dispatcher::new();
        d.collect('#');
        d.dispatch(FunctionType::Escape, '8', &mut fb);
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(fb.cell(y, x).unwrap().contents(), "E");
            }
        }
    }

    #[test]
    fn osc_sets_window_title_and_icon_name() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "0;hello".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert_eq!(fb.window_title(), &['h', 'e', 'l', 'l', 'o']);
        assert_eq!(fb.icon_name(), &['h', 'e', 'l', 'l', 'o']);
        assert!(fb.is_title_initialized());
    }

    #[test]
    fn osc_2_sets_only_the_title() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "2;t".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert_eq!(fb.window_title(), &['t']);
        assert!(fb.icon_name().is_empty());
    }

    #[test]
    fn osc_52_sets_the_clipboard() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "52;c;aGk=".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert_eq!(fb.clipboard(), &['c', ';', 'a', 'G', 'k', '=']);
        assert_eq!(fb.clipboard_seq(), 1);
    }

    #[test]
    fn osc_8_sets_and_clears_a_hyperlink() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "8;id=1;http://x".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert!(!fb.ds.hyperlink().is_empty());
        assert_eq!(fb.ds.hyperlink().osc8(), "\x1b]8;id=1;http://x\x1b\\");

        // An empty URL closes the link.
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "8;;".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert!(fb.ds.hyperlink().is_empty());
    }

    #[test]
    fn malformed_osc_8_is_ignored() {
        let mut fb = Framebuffer::new(10, 2);
        for payload in ["8", "8;", "8;onlyone"] {
            let mut d = Dispatcher::new();
            d.osc_start();
            for c in payload.chars() {
                d.osc_put(c);
            }
            d.osc_dispatch(&mut fb);
            assert!(fb.ds.hyperlink().is_empty(), "{payload} set a hyperlink");
        }
    }

    #[test]
    fn colour_queries_are_forwarded_verbatim() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "4;1;?".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert_eq!(fb.color_queries(), "\x1b]4;1;?\x1b\\");

        // A dynamic colour query, e.g. the default background.
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "11;?".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert_eq!(fb.color_queries(), "\x1b]11;?\x1b\\");
    }

    #[test]
    fn a_non_query_osc_is_not_treated_as_a_colour_query() {
        let mut fb = Framebuffer::new(10, 2);
        let mut d = Dispatcher::new();
        d.osc_start();
        for c in "4;1;rgb:00/00/00".chars() {
            d.osc_put(c);
        }
        d.osc_dispatch(&mut fb);
        assert!(fb.color_queries().is_empty());
    }

    #[test]
    fn control_characters_move_the_cursor() {
        let mut fb = Framebuffer::new(10, 5);
        fb.ds.move_col(5, false, false);

        let mut d = Dispatcher::new();
        d.dispatch(FunctionType::Control, '\r', &mut fb);
        assert_eq!(fb.ds.cursor_col(), 0);

        let mut d = Dispatcher::new();
        d.dispatch(FunctionType::Control, '\n', &mut fb);
        assert_eq!(fb.ds.cursor_row(), 1);

        let mut d = Dispatcher::new();
        d.dispatch(FunctionType::Control, '\x07', &mut fb);
        assert_eq!(fb.bell_count(), 1);
    }

    #[test]
    fn erase_character_stops_at_the_screen_edge() {
        let mut fb = Framebuffer::new(5, 1);
        for c in 0..5 {
            fb.mutable_cell(0, c).append('x');
        }
        fb.ds.move_col(3, false, false);
        dispatch_csi(&mut fb, "100", 'X');
        assert_eq!(fb.cell(0, 2).unwrap().contents(), "x");
        assert!(fb.cell(0, 3).unwrap().is_empty());
        assert!(fb.cell(0, 4).unwrap().is_empty());
    }
}
