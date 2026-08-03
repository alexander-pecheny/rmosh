//! The screen: cells, rows, cursor state, and the framebuffer that holds them.
//!
//! Transliterated from `third_party/mosh/src/terminal/terminalframebuffer.{h,cc}`.
//!
//! Rows are shared between framebuffers and copied only when written to, which is what
//! makes taking a snapshot of the screen cheap. The C++ does this with `shared_ptr` and
//! an explicit `use_count() > 1` check; `Rc::make_mut` is the same thing with the check
//! built in.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type ColorType = u32;

/// Set in a color value to mark it as 24-bit rather than a palette index.
const TRUE_COLOR_MASK: u32 = 0x1000000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute {
    Bold,
    Faint,
    Italic,
    Underlined,
    Blink,
    Inverse,
    Invisible,
    Strikethrough,
}

impl Attribute {
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Colour and attributes for one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renditions {
    foreground_color: ColorType,
    background_color: ColorType,
    attributes: u8,
}

impl Renditions {
    pub fn new(background: ColorType) -> Self {
        Renditions {
            foreground_color: 0,
            background_color: background,
            attributes: 0,
        }
    }

    pub fn make_true_color(r: u32, g: u32, b: u32) -> ColorType {
        TRUE_COLOR_MASK | (r << 16) | (g << 8) | b
    }

    pub fn is_true_color(color: ColorType) -> bool {
        color & TRUE_COLOR_MASK != 0
    }

    pub fn background_rendition(&self) -> ColorType {
        self.background_color
    }

    pub fn set_attribute(&mut self, attr: Attribute, val: bool) {
        if val {
            self.attributes |= attr.bit();
        } else {
            self.attributes &= !attr.bit();
        }
    }

    pub fn attribute(&self, attr: Attribute) -> bool {
        self.attributes & attr.bit() != 0
    }

    pub fn clear_attributes(&mut self) {
        self.attributes = 0;
    }

    pub fn set_foreground_color(&mut self, num: i32) {
        if (0..=255).contains(&num) {
            self.foreground_color = 30 + num as ColorType;
        } else if num >= 0 && Self::is_true_color(num as ColorType) {
            self.foreground_color = num as ColorType;
        }
    }

    pub fn set_background_color(&mut self, num: i32) {
        if (0..=255).contains(&num) {
            self.background_color = 40 + num as ColorType;
        } else if num >= 0 && Self::is_true_color(num as ColorType) {
            self.background_color = num as ColorType;
        }
    }

    /// Apply one SGR parameter. Cannot set colours beyond the 16-colour set; those
    /// arrive through `set_foreground_color`/`set_background_color`.
    pub fn set_rendition(&mut self, num: ColorType) {
        if num == 0 {
            self.clear_attributes();
            self.foreground_color = 0;
            self.background_color = 0;
            return;
        }

        if num == 39 {
            self.foreground_color = 0;
            return;
        } else if num == 49 {
            self.background_color = 0;
            return;
        }

        if (30..=37).contains(&num) {
            self.foreground_color = num;
            return;
        } else if (40..=47).contains(&num) {
            self.background_color = num;
            return;
        } else if (90..=97).contains(&num) {
            self.foreground_color = num - 90 + 38;
            return;
        } else if (100..=107).contains(&num) {
            self.background_color = num - 100 + 48;
            return;
        }

        let value = num < 10;
        match num {
            1 => self.set_attribute(Attribute::Bold, value),
            2 => self.set_attribute(Attribute::Faint, value),
            22 => {
                self.set_attribute(Attribute::Bold, value);
                self.set_attribute(Attribute::Faint, value);
            }
            3 | 23 => self.set_attribute(Attribute::Italic, value),
            4 | 24 => self.set_attribute(Attribute::Underlined, value),
            5 | 25 => self.set_attribute(Attribute::Blink, value),
            7 | 27 => self.set_attribute(Attribute::Inverse, value),
            8 | 28 => self.set_attribute(Attribute::Invisible, value),
            9 | 29 => self.set_attribute(Attribute::Strikethrough, value),
            _ => {} // ignore unknown rendition
        }
    }

    /// The escape sequence that puts a terminal into these renditions.
    ///
    /// Goes to the user's real terminal, so the exact spelling is observable and must
    /// match the C++ byte for byte.
    pub fn sgr(&self) -> String {
        let mut ret = String::from("\x1b[0");

        for (attr, code) in [
            (Attribute::Bold, "1"),
            (Attribute::Faint, "2"),
            (Attribute::Italic, "3"),
            (Attribute::Underlined, "4"),
            (Attribute::Blink, "5"),
            (Attribute::Inverse, "7"),
            (Attribute::Invisible, "8"),
            (Attribute::Strikethrough, "9"),
        ] {
            if self.attribute(attr) {
                ret.push(';');
                ret.push_str(code);
            }
        }

        if self.foreground_color != 0 {
            let c = self.foreground_color;
            if Self::is_true_color(c) {
                ret.push_str(&format!(
                    ";38;2;{};{};{}",
                    (c >> 16) & 0xff,
                    (c >> 8) & 0xff,
                    c & 0xff
                ));
            } else if c > 37 {
                ret.push_str(&format!(";38;5;{}", c - 30));
            } else {
                ret.push_str(&format!(";{c}"));
            }
        }
        if self.background_color != 0 {
            let c = self.background_color;
            if Self::is_true_color(c) {
                ret.push_str(&format!(
                    ";48;2;{};{};{}",
                    (c >> 16) & 0xff,
                    (c >> 8) & 0xff,
                    c & 0xff
                ));
            } else if c > 47 {
                ret.push_str(&format!(";48;5;{}", c - 40));
            } else {
                ret.push_str(&format!(";{c}"));
            }
        }
        ret.push('m');
        ret
    }
}

#[derive(Debug, PartialEq, Eq)]
struct HyperlinkRep {
    params: String,
    url: String,
}

/// An OSC 8 hyperlink attached to a cell. Shared between cells of the same link.
#[derive(Debug, Clone, Default)]
pub struct Hyperlink {
    rep: Option<Rc<HyperlinkRep>>,
}

impl Hyperlink {
    pub fn new(params: String, url: String) -> Self {
        // An empty URL means "no link", which is how a link is closed.
        if url.is_empty() {
            Hyperlink { rep: None }
        } else {
            Hyperlink {
                rep: Some(Rc::new(HyperlinkRep { params, url })),
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rep.is_none()
    }

    /// The escape sequence that opens (or closes) this link.
    pub fn osc8(&self) -> String {
        let mut ret = String::from("\x1b]8;");
        if let Some(rep) = &self.rep {
            ret.push_str(&rep.params);
            ret.push(';');
            ret.push_str(&rep.url);
        } else {
            ret.push(';');
        }
        ret.push_str("\x1b\\");
        ret
    }
}

impl PartialEq for Hyperlink {
    fn eq(&self, other: &Self) -> bool {
        match (&self.rep, &other.rep) {
            // Same allocation is the common case and needs no comparison.
            (Some(a), Some(b)) => Rc::ptr_eq(a, b) || a == b,
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for Hyperlink {}

/// A limit on combining characters, so one cell cannot grow without bound.
const MAX_CELL_BYTES: usize = 32;

/// One addressable position on the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    contents: String,
    renditions: Renditions,
    hyperlink: Hyperlink,
    wide: bool,
    /// The cell begins with a combining character, so it is drawn on a no-break space.
    fallback: bool,
    wrap: bool,
}

impl Cell {
    pub fn new(background_color: ColorType) -> Self {
        Cell {
            contents: String::new(),
            renditions: Renditions::new(background_color),
            hyperlink: Hyperlink::default(),
            wide: false,
            fallback: false,
            wrap: false,
        }
    }

    pub fn reset(&mut self, background_color: ColorType) {
        self.contents.clear();
        self.renditions = Renditions::new(background_color);
        self.hyperlink = Hyperlink::default();
        self.wide = false;
        self.fallback = false;
        self.wrap = false;
    }

    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.contents.len() >= MAX_CELL_BYTES
    }

    pub fn clear(&mut self) {
        self.contents.clear();
    }

    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// A blank cell: empty, a space, or a no-break space.
    pub fn is_blank(&self) -> bool {
        self.contents.is_empty() || self.contents == " " || self.contents == "\u{a0}"
    }

    pub fn contents_match(&self, other: &Cell) -> bool {
        (self.is_blank() && other.is_blank()) || self.contents == other.contents
    }

    /// Is this a printing ISO 8859-1 character? A cheap test for common narrow text.
    pub fn isprint_iso8859_1(c: char) -> bool {
        let c = c as u32;
        (0xa0..=0xff).contains(&c) || (0x20..=0x7e).contains(&c)
    }

    pub fn append(&mut self, c: char) {
        // ASCII? Skip the locale encoder.
        if (c as u32) <= 0x7f {
            self.contents.push(c);
            return;
        }
        let mut bytes = Vec::new();
        mosh_sys::append_wchar(&mut bytes, c);
        if let Ok(s) = std::str::from_utf8(&bytes) {
            self.contents.push_str(s);
        }
    }

    /// Append what this cell displays, as it should be sent to a terminal.
    pub fn print_grapheme(&self, out: &mut String) {
        if self.contents.is_empty() {
            out.push(' ');
            return;
        }
        // A cell starting with a combining character needs something to combine with.
        if self.fallback {
            out.push('\u{a0}');
        }
        out.push_str(&self.contents);
    }

    pub fn hyperlink(&self) -> &Hyperlink {
        &self.hyperlink
    }

    pub fn set_hyperlink(&mut self, l: Hyperlink) {
        self.hyperlink = l;
    }

    pub fn renditions(&self) -> &Renditions {
        &self.renditions
    }

    pub fn renditions_mut(&mut self) -> &mut Renditions {
        &mut self.renditions
    }

    pub fn set_renditions(&mut self, r: Renditions) {
        self.renditions = r;
    }

    pub fn wide(&self) -> bool {
        self.wide
    }

    pub fn set_wide(&mut self, w: bool) {
        self.wide = w;
    }

    pub fn width(&self) -> u32 {
        self.wide as u32 + 1
    }

    pub fn fallback(&self) -> bool {
        self.fallback
    }

    pub fn set_fallback(&mut self, f: bool) {
        self.fallback = f;
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn set_wrap(&mut self, f: bool) {
        self.wrap = f;
    }

    /// Debug rendering used when comparing states, mirroring `Cell::debug_contents`.
    pub fn debug_contents(&self) -> String {
        if self.contents.is_empty() {
            return "'_' ()".to_string();
        }
        let mut chars = String::from("'");
        self.print_grapheme(&mut chars);
        chars.push_str("' [");
        let mut first = true;
        for b in self.contents.as_bytes() {
            if !first {
                chars.push_str(", ");
            }
            chars.push_str(&format!("0x{b:02x}"));
            first = false;
        }
        chars.push(']');
        chars
    }
}

/// Hands out a new generation number for each row created or reset.
///
/// Two rows with different generations cannot be equal, which lets scrolling rule out
/// most row comparisons without looking at any cells.
static GEN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_gen() -> u64 {
    GEN_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// One line of the screen.
#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Generation counter; see [`GEN_COUNTER`].
    pub gen: u64,
}

impl Row {
    pub fn new(width: usize, background_color: ColorType) -> Self {
        Row {
            cells: vec![Cell::new(background_color); width],
            gen: next_gen(),
        }
    }

    pub fn insert_cell(&mut self, col: usize, background_color: ColorType) {
        self.cells.insert(col, Cell::new(background_color));
        self.cells.pop();
    }

    pub fn delete_cell(&mut self, col: usize, background_color: ColorType) {
        self.cells.push(Cell::new(background_color));
        self.cells.remove(col);
    }

    pub fn reset(&mut self, background_color: ColorType) {
        self.gen = next_gen();
        for cell in &mut self.cells {
            cell.reset(background_color);
        }
    }

    pub fn wrap(&self) -> bool {
        self.cells.last().map(|c| c.wrap()).unwrap_or(false)
    }

    pub fn set_wrap(&mut self, w: bool) {
        if let Some(last) = self.cells.last_mut() {
            last.set_wrap(w);
        }
    }
}

impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.gen == other.gen && self.cells == other.cells
    }
}

impl Eq for Row {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedCursor {
    pub cursor_col: i32,
    pub cursor_row: i32,
    pub renditions: Renditions,
    pub auto_wrap_mode: bool,
    pub origin_mode: bool,
}

impl Default for SavedCursor {
    fn default() -> Self {
        SavedCursor {
            cursor_col: 0,
            cursor_row: 0,
            renditions: Renditions::new(0),
            auto_wrap_mode: true,
            origin_mode: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseReportingMode {
    None = 0,
    X10 = 9,
    Vt220 = 1000,
    Vt220Hilight = 1001,
    BtnEvent = 1002,
    AnyEvent = 1003,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncodingMode {
    Default = 0,
    Utf8 = 1005,
    Sgr = 1006,
    Urxvt = 1015,
}

/// Everything about the screen that is not the cells themselves.
#[derive(Debug, Clone)]
pub struct DrawState {
    width: i32,
    height: i32,

    cursor_col: i32,
    cursor_row: i32,
    combining_char_col: i32,
    combining_char_row: i32,

    default_tabs: bool,
    tabs: Vec<bool>,

    scrolling_region_top_row: i32,
    scrolling_region_bottom_row: i32,

    renditions: Renditions,
    hyperlink: Hyperlink,

    save: SavedCursor,

    pub next_print_will_wrap: bool,
    pub origin_mode: bool,
    pub auto_wrap_mode: bool,
    pub insert_mode: bool,
    pub cursor_visible: bool,
    pub reverse_video: bool,
    pub bracketed_paste: bool,

    pub mouse_reporting_mode: MouseReportingMode,
    pub mouse_focus_event: bool,
    pub mouse_alternate_scroll: bool,
    pub mouse_encoding_mode: MouseEncodingMode,

    pub application_mode_cursor_keys: bool,
}

impl DrawState {
    pub fn new(width: i32, height: i32) -> Self {
        let mut ds = DrawState {
            width,
            height,
            cursor_col: 0,
            cursor_row: 0,
            combining_char_col: 0,
            combining_char_row: 0,
            default_tabs: true,
            tabs: vec![false; width.max(0) as usize],
            scrolling_region_top_row: 0,
            scrolling_region_bottom_row: height - 1,
            renditions: Renditions::new(0),
            hyperlink: Hyperlink::default(),
            save: SavedCursor::default(),
            next_print_will_wrap: false,
            origin_mode: false,
            auto_wrap_mode: true,
            insert_mode: false,
            cursor_visible: true,
            reverse_video: false,
            bracketed_paste: false,
            mouse_reporting_mode: MouseReportingMode::None,
            mouse_focus_event: false,
            mouse_alternate_scroll: false,
            mouse_encoding_mode: MouseEncodingMode::Default,
            application_mode_cursor_keys: false,
        };
        ds.reinitialize_tabs(0);
        ds
    }

    fn reinitialize_tabs(&mut self, start: usize) {
        debug_assert!(self.default_tabs);
        for i in start..self.tabs.len() {
            self.tabs[i] = i % 8 == 0;
        }
    }

    fn new_grapheme(&mut self) {
        self.combining_char_col = self.cursor_col;
        self.combining_char_row = self.cursor_row;
    }

    fn snap_cursor_to_border(&mut self) {
        self.cursor_row = self.cursor_row.clamp(self.limit_top(), self.limit_bottom());
        self.cursor_col = self.cursor_col.clamp(0, self.width - 1);
    }

    pub fn move_row(&mut self, n: i32, relative: bool) {
        if relative {
            self.cursor_row += n;
        } else {
            self.cursor_row = n + self.limit_top();
        }
        self.snap_cursor_to_border();
        self.new_grapheme();
        self.next_print_will_wrap = false;
    }

    pub fn move_col(&mut self, n: i32, relative: bool, implicit: bool) {
        if implicit {
            self.new_grapheme();
        }
        if relative {
            self.cursor_col += n;
        } else {
            self.cursor_col = n;
        }
        if implicit {
            self.next_print_will_wrap = self.cursor_col >= self.width;
        }
        self.snap_cursor_to_border();
        if !implicit {
            self.new_grapheme();
            self.next_print_will_wrap = false;
        }
    }

    pub fn cursor_col(&self) -> i32 {
        self.cursor_col
    }
    pub fn cursor_row(&self) -> i32 {
        self.cursor_row
    }
    pub fn combining_char_col(&self) -> i32 {
        self.combining_char_col
    }
    pub fn combining_char_row(&self) -> i32 {
        self.combining_char_row
    }
    pub fn width(&self) -> i32 {
        self.width
    }
    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn set_tab(&mut self) {
        let col = self.cursor_col as usize;
        if col < self.tabs.len() {
            self.tabs[col] = true;
        }
    }

    pub fn clear_tab(&mut self, col: i32) {
        if let Some(slot) = usize::try_from(col).ok().and_then(|c| self.tabs.get_mut(c)) {
            *slot = false;
        }
    }

    /// Default tabs cannot be restored without resetting the draw state.
    pub fn clear_default_tabs(&mut self) {
        self.default_tabs = false;
    }

    pub fn next_tab(&self, mut count: i32) -> i32 {
        if count >= 0 {
            for i in (self.cursor_col + 1)..self.width {
                if self.tabs[i as usize] {
                    count -= 1;
                    if count == 0 {
                        return i;
                    }
                }
            }
            return -1;
        }
        let mut i = self.cursor_col - 1;
        while i > 0 {
            if self.tabs[i as usize] {
                count += 1;
                if count == 0 {
                    return i;
                }
            }
            i -= 1;
        }
        0
    }

    pub fn set_scrolling_region(&mut self, top: i32, bottom: i32) {
        if self.height < 1 {
            return;
        }
        self.scrolling_region_top_row = top;
        self.scrolling_region_bottom_row = bottom;

        if self.scrolling_region_top_row < 0 {
            self.scrolling_region_top_row = 0;
        }
        if self.scrolling_region_bottom_row >= self.height {
            self.scrolling_region_bottom_row = self.height - 1;
        }
        if self.scrolling_region_bottom_row < self.scrolling_region_top_row {
            self.scrolling_region_bottom_row = self.scrolling_region_top_row;
        }
        // The real rule requires a two-line scrolling region; we do not enforce it.

        if self.origin_mode {
            self.snap_cursor_to_border();
            self.new_grapheme();
        }
    }

    pub fn scrolling_region_top_row(&self) -> i32 {
        self.scrolling_region_top_row
    }

    pub fn scrolling_region_bottom_row(&self) -> i32 {
        self.scrolling_region_bottom_row
    }

    pub fn limit_top(&self) -> i32 {
        if self.origin_mode {
            self.scrolling_region_top_row
        } else {
            0
        }
    }

    pub fn limit_bottom(&self) -> i32 {
        if self.origin_mode {
            self.scrolling_region_bottom_row
        } else {
            self.height - 1
        }
    }

    pub fn hyperlink(&self) -> &Hyperlink {
        &self.hyperlink
    }

    pub fn set_hyperlink(&mut self, x: Hyperlink) {
        self.hyperlink = x;
    }

    pub fn set_foreground_color(&mut self, x: i32) {
        self.renditions.set_foreground_color(x);
    }

    pub fn set_background_color(&mut self, x: i32) {
        self.renditions.set_background_color(x);
    }

    pub fn add_rendition(&mut self, x: ColorType) {
        self.renditions.set_rendition(x);
    }

    pub fn renditions(&self) -> &Renditions {
        &self.renditions
    }

    pub fn renditions_mut(&mut self) -> &mut Renditions {
        &mut self.renditions
    }

    pub fn background_rendition(&self) -> ColorType {
        self.renditions.background_rendition()
    }

    pub fn save_cursor(&mut self) {
        self.save = SavedCursor {
            cursor_col: self.cursor_col,
            cursor_row: self.cursor_row,
            renditions: self.renditions,
            auto_wrap_mode: self.auto_wrap_mode,
            origin_mode: self.origin_mode,
        };
    }

    pub fn restore_cursor(&mut self) {
        self.cursor_col = self.save.cursor_col;
        self.cursor_row = self.save.cursor_row;
        self.renditions = self.save.renditions;
        self.auto_wrap_mode = self.save.auto_wrap_mode;
        self.origin_mode = self.save.origin_mode;

        self.snap_cursor_to_border(); // the screen could have been resized meanwhile
        self.new_grapheme();
    }

    pub fn clear_saved_cursor(&mut self) {
        self.save = SavedCursor::default();
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        if self.width != width || self.height != height {
            // xterm and rxvt-unicode reset the whole scrolling region on any resize;
            // gnome-terminal only shrinks it. We follow xterm.
            self.scrolling_region_top_row = 0;
            self.scrolling_region_bottom_row = height - 1;
        }

        self.tabs.resize(width.max(0) as usize, false);
        if self.default_tabs {
            let from = self.width.max(0) as usize;
            self.reinitialize_tabs(from);
        }

        self.width = width;
        self.height = height;

        self.snap_cursor_to_border();

        // The saved cursor is snapped on restore instead.

        if self.combining_char_col >= self.width || self.combining_char_row >= self.height {
            self.combining_char_col = -1;
            self.combining_char_row = -1;
        }
    }
}

impl PartialEq for DrawState {
    /// Compares only what affects the display, as the C++ does.
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.cursor_col == other.cursor_col
            && self.cursor_row == other.cursor_row
            && self.cursor_visible == other.cursor_visible
            && self.reverse_video == other.reverse_video
            && self.renditions == other.renditions
            && self.bracketed_paste == other.bracketed_paste
            && self.mouse_reporting_mode == other.mouse_reporting_mode
            && self.mouse_focus_event == other.mouse_focus_event
            && self.mouse_alternate_scroll == other.mouse_alternate_scroll
            && self.mouse_encoding_mode == other.mouse_encoding_mode
            && self.hyperlink == other.hyperlink
    }
}

impl Eq for DrawState {}

/// A limit on how much colour-query output we will buffer for one batch.
pub const MAXIMUM_COLOR_QUERIES: usize = 4096;

/// The screen: rows of cells plus the draw state.
#[derive(Debug, Clone)]
pub struct Framebuffer {
    rows: Vec<Rc<Row>>,
    icon_name: Vec<char>,
    window_title: Vec<char>,
    clipboard: Vec<char>,
    clipboard_seq: u32,
    color_queries: String,
    color_query_seq: u32,
    bell_count: u32,
    /// True once the window title has been set by an escape sequence.
    title_initialized: bool,
    pub ds: DrawState,
}

impl Framebuffer {
    pub fn new(width: i32, height: i32) -> Self {
        assert!(width > 0 && height > 0);
        let row = Rc::new(Row::new(width as usize, 0));
        Framebuffer {
            rows: vec![row; height as usize],
            icon_name: Vec::new(),
            window_title: Vec::new(),
            clipboard: Vec::new(),
            clipboard_seq: 0,
            color_queries: String::new(),
            color_query_seq: 0,
            bell_count: 0,
            title_initialized: false,
            ds: DrawState::new(width, height),
        }
    }

    fn newrow(&self) -> Rc<Row> {
        Rc::new(Row::new(
            self.ds.width() as usize,
            self.ds.background_rendition(),
        ))
    }

    pub fn rows(&self) -> &[Rc<Row>] {
        &self.rows
    }

    pub fn scroll(&mut self, n: i32) {
        if n >= 0 {
            self.delete_line(self.ds.scrolling_region_top_row(), n);
        } else {
            self.insert_line(self.ds.scrolling_region_top_row(), -n);
        }
    }

    pub fn move_rows_autoscroll(&mut self, rows: i32) {
        // Do not scroll if the cursor is outside the scrolling region.
        if self.ds.cursor_row() < self.ds.scrolling_region_top_row()
            || self.ds.cursor_row() > self.ds.scrolling_region_bottom_row()
        {
            self.ds.move_row(rows, true);
            return;
        }

        if self.ds.cursor_row() + rows > self.ds.scrolling_region_bottom_row() {
            let n = self.ds.cursor_row() + rows - self.ds.scrolling_region_bottom_row();
            self.scroll(n);
            self.ds.move_row(-n, true);
        } else if self.ds.cursor_row() + rows < self.ds.scrolling_region_top_row() {
            let n = self.ds.cursor_row() + rows - self.ds.scrolling_region_top_row();
            self.scroll(n);
            self.ds.move_row(-n, true);
        }

        self.ds.move_row(rows, true);
    }

    pub fn row(&self, row: i32) -> Option<&Row> {
        let row = if row == -1 { self.ds.cursor_row() } else { row };
        self.rows
            .get(usize::try_from(row).ok()?)
            .map(|r| r.as_ref())
    }

    pub fn cell(&self, row: i32, col: i32) -> Option<&Cell> {
        let row = if row == -1 { self.ds.cursor_row() } else { row };
        let col = if col == -1 { self.ds.cursor_col() } else { col };
        self.rows
            .get(usize::try_from(row).ok()?)?
            .cells
            .get(usize::try_from(col).ok()?)
    }

    /// A row that can be written to, copied first if any other framebuffer shares it.
    pub fn mutable_row(&mut self, row: i32) -> &mut Row {
        let row = if row == -1 { self.ds.cursor_row() } else { row };
        Rc::make_mut(&mut self.rows[row as usize])
    }

    pub fn mutable_cell(&mut self, row: i32, col: i32) -> &mut Cell {
        let col = if col == -1 { self.ds.cursor_col() } else { col };
        &mut self.mutable_row(row).cells[col as usize]
    }

    /// The cell a combining character should attach to, if it is still on screen.
    pub fn combining_cell(&mut self) -> Option<&mut Cell> {
        let (col, row) = (self.ds.combining_char_col(), self.ds.combining_char_row());
        // A resize since the base character was printed can put this off screen.
        if col < 0 || row < 0 || col >= self.ds.width() || row >= self.ds.height() {
            return None;
        }
        Some(self.mutable_cell(row, col))
    }

    pub fn apply_renditions_to_cell(&mut self, row: i32, col: i32) {
        let r = *self.ds.renditions();
        self.mutable_cell(row, col).set_renditions(r);
    }

    pub fn apply_hyperlink_to_cell(&mut self, row: i32, col: i32) {
        let h = self.ds.hyperlink().clone();
        self.mutable_cell(row, col).set_hyperlink(h);
    }

    pub fn insert_line(&mut self, before_row: i32, count: i32) {
        if before_row < self.ds.scrolling_region_top_row()
            || before_row > self.ds.scrolling_region_bottom_row() + 1
        {
            return;
        }

        let mut scroll = self.ds.scrolling_region_bottom_row() + 1 - before_row;
        if count < scroll {
            scroll = count;
        }
        if scroll <= 0 {
            return;
        }

        let start = (self.ds.scrolling_region_bottom_row() + 1 - scroll) as usize;
        self.rows.drain(start..start + scroll as usize);

        let blank = self.newrow();
        let at = before_row as usize;
        self.rows
            .splice(at..at, std::iter::repeat_n(blank, scroll as usize));
    }

    pub fn delete_line(&mut self, row: i32, count: i32) {
        if row < self.ds.scrolling_region_top_row() || row > self.ds.scrolling_region_bottom_row() {
            return;
        }

        let mut scroll = self.ds.scrolling_region_bottom_row() + 1 - row;
        if count < scroll {
            scroll = count;
        }
        if scroll <= 0 {
            return;
        }

        self.rows.drain(row as usize..(row + scroll) as usize);

        let blank = self.newrow();
        let at = (self.ds.scrolling_region_bottom_row() + 1 - scroll) as usize;
        self.rows
            .splice(at..at, std::iter::repeat_n(blank, scroll as usize));
    }

    pub fn insert_cell(&mut self, row: i32, col: i32) {
        let bg = self.ds.background_rendition();
        self.mutable_row(row).insert_cell(col as usize, bg);
    }

    pub fn delete_cell(&mut self, row: i32, col: i32) {
        let bg = self.ds.background_rendition();
        self.mutable_row(row).delete_cell(col as usize, bg);
    }

    pub fn reset(&mut self) {
        let (width, height) = (self.ds.width(), self.ds.height());
        self.ds = DrawState::new(width, height);
        let row = self.newrow();
        self.rows = vec![row; height as usize];
        self.window_title.clear();
        self.clipboard.clear();
        self.color_queries.clear();
        // bell_count deliberately survives a reset.
    }

    pub fn soft_reset(&mut self) {
        self.ds.insert_mode = false;
        self.ds.origin_mode = false;
        self.ds.cursor_visible = true; // per xterm and gnome-terminal
        self.ds.application_mode_cursor_keys = false;
        self.ds.set_scrolling_region(0, self.ds.height() - 1);
        self.ds.add_rendition(0);
        self.ds.set_hyperlink(Hyperlink::default());
        self.ds.clear_saved_cursor();
    }

    pub fn set_title_initialized(&mut self) {
        self.title_initialized = true;
    }

    pub fn is_title_initialized(&self) -> bool {
        self.title_initialized
    }

    pub fn set_icon_name(&mut self, s: Vec<char>) {
        self.icon_name = s;
    }

    pub fn set_window_title(&mut self, s: Vec<char>) {
        self.window_title = s;
    }

    pub fn set_clipboard(&mut self, s: Vec<char>) {
        self.clipboard = s;
        self.clipboard_seq += 1;
    }

    pub fn icon_name(&self) -> &[char] {
        &self.icon_name
    }

    pub fn window_title(&self) -> &[char] {
        &self.window_title
    }

    pub fn clipboard(&self) -> &[char] {
        &self.clipboard
    }

    pub fn clipboard_seq(&self) -> u32 {
        self.clipboard_seq
    }

    /// Colour queries are events rather than state: they accumulate for one batch of
    /// terminal output and are forwarded to the client's terminal once.
    pub fn add_color_query(&mut self, query: &str) {
        if self.color_queries.len() + query.len() <= MAXIMUM_COLOR_QUERIES {
            self.color_queries.push_str(query);
        }
        self.color_query_seq += 1;
    }

    pub fn clear_color_queries(&mut self) {
        self.color_queries.clear();
    }

    pub fn color_queries(&self) -> &str {
        &self.color_queries
    }

    pub fn color_query_seq(&self) -> u32 {
        self.color_query_seq
    }

    pub fn prefix_window_title(&mut self, s: &[char]) {
        if self.icon_name == self.window_title {
            // Preserve the equivalence rather than letting them drift apart.
            self.icon_name.splice(0..0, s.iter().copied());
        }
        self.window_title.splice(0..0, s.iter().copied());
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        assert!(width > 0 && height > 0);

        let oldheight = self.ds.height();
        let oldwidth = self.ds.width();
        self.ds.resize(width, height);

        let blankrow = self.newrow();
        if oldheight != height {
            self.rows.resize(height as usize, blankrow.clone());
        }
        if oldwidth == width {
            return;
        }
        let bg = self.ds.background_rendition();
        for r in self.rows.iter_mut() {
            if Rc::ptr_eq(r, &blankrow) {
                break;
            }
            let row = Rc::make_mut(r);
            row.set_wrap(false);
            row.cells.resize(width as usize, Cell::new(bg));
        }
    }

    pub fn reset_cell(&mut self, row: i32, col: i32) {
        let bg = self.ds.background_rendition();
        self.mutable_cell(row, col).reset(bg);
    }

    pub fn reset_row(&mut self, row: i32) {
        let bg = self.ds.background_rendition();
        self.mutable_row(row).reset(bg);
    }

    pub fn ring_bell(&mut self) {
        self.bell_count += 1;
    }

    pub fn bell_count(&self) -> u32 {
        self.bell_count
    }
}

impl PartialEq for Framebuffer {
    fn eq(&self, other: &Self) -> bool {
        self.rows.len() == other.rows.len()
            // Rows are compared by identity, not by content. The C++ compares a
            // vector<shared_ptr<Row>>, which compares the pointers, and that is the
            // intended meaning: this asks "is this the same row I already had", which is
            // what state sync needs. Two rows with equal contents but separate
            // allocations are deliberately unequal -- the cost is a redundant diff,
            // whereas a false positive would lose an update.
            && self.rows.iter().zip(&other.rows).all(|(a, b)| Rc::ptr_eq(a, b))
            && self.window_title == other.window_title
            && self.clipboard == other.clipboard
            && self.clipboard_seq == other.clipboard_seq
            && self.color_queries == other.color_queries
            && self.color_query_seq == other.color_query_seq
            && self.bell_count == other.bell_count
            && self.ds == other.ds
    }
}

impl Eq for Framebuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_matches_the_documented_spelling() {
        let mut r = Renditions::new(0);
        assert_eq!(r.sgr(), "\x1b[0m");

        r.set_attribute(Attribute::Bold, true);
        assert_eq!(r.sgr(), "\x1b[0;1m");

        r.set_attribute(Attribute::Faint, true);
        r.set_attribute(Attribute::Strikethrough, true);
        assert_eq!(r.sgr(), "\x1b[0;1;2;9m");
    }

    #[test]
    fn sgr_renders_each_colour_family() {
        // ANSI colours pass through as their own code.
        let mut r = Renditions::new(0);
        r.set_rendition(31);
        assert_eq!(r.sgr(), "\x1b[0;31m");

        // 256-colour indices above the ANSI range use the 38;5 form.
        let mut r = Renditions::new(0);
        r.set_foreground_color(200);
        assert_eq!(r.sgr(), "\x1b[0;38;5;200m");

        // True colour uses 38;2 with three components.
        let mut r = Renditions::new(0);
        r.set_foreground_color(Renditions::make_true_color(1, 2, 3) as i32);
        assert_eq!(r.sgr(), "\x1b[0;38;2;1;2;3m");

        // Backgrounds mirror foregrounds.
        let mut r = Renditions::new(0);
        r.set_background_color(200);
        assert_eq!(r.sgr(), "\x1b[0;48;5;200m");
    }

    #[test]
    fn rendition_zero_clears_everything() {
        let mut r = Renditions::new(0);
        r.set_attribute(Attribute::Bold, true);
        r.set_rendition(31);
        r.set_rendition(0);
        assert_eq!(r.sgr(), "\x1b[0m");
    }

    #[test]
    fn rendition_22_clears_bold_and_faint_together() {
        let mut r = Renditions::new(0);
        r.set_rendition(1);
        r.set_rendition(2);
        r.set_rendition(22);
        assert!(!r.attribute(Attribute::Bold));
        assert!(!r.attribute(Attribute::Faint));
    }

    #[test]
    fn bright_colours_map_into_the_256_colour_space() {
        let mut r = Renditions::new(0);
        r.set_rendition(91); // bright red foreground
        assert_eq!(r.sgr(), "\x1b[0;38;5;9m");
    }

    #[test]
    fn hyperlink_equality_compares_content_not_identity() {
        let a = Hyperlink::new("id=1".into(), "http://x".into());
        let b = Hyperlink::new("id=1".into(), "http://x".into());
        let c = Hyperlink::new("id=2".into(), "http://x".into());
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, Hyperlink::default());
    }

    #[test]
    fn an_empty_url_means_no_link() {
        let h = Hyperlink::new("id=1".into(), String::new());
        assert!(h.is_empty());
        assert_eq!(h.osc8(), "\x1b]8;;\x1b\\");
    }

    #[test]
    fn hyperlink_osc8_round_trip() {
        let h = Hyperlink::new("id=1".into(), "http://example.com".into());
        assert_eq!(h.osc8(), "\x1b]8;id=1;http://example.com\x1b\\");
    }

    #[test]
    fn blank_cells_compare_equal_however_they_are_spelled() {
        let mut empty = Cell::new(0);
        let mut space = Cell::new(0);
        space.append(' ');
        let mut nbsp = Cell::new(0);
        nbsp.append('\u{a0}');

        assert!(empty.is_blank() && space.is_blank() && nbsp.is_blank());
        assert!(empty.contents_match(&space));
        assert!(space.contents_match(&nbsp));

        empty.append('x');
        assert!(!empty.contents_match(&space));
    }

    #[test]
    fn a_cell_stops_accepting_combining_characters() {
        let mut c = Cell::new(0);
        for _ in 0..40 {
            if !c.is_full() {
                c.append('a');
            }
        }
        assert!(c.is_full());
        assert!(c.contents().len() <= MAX_CELL_BYTES + 4);
    }

    #[test]
    fn rows_get_distinct_generations() {
        let a = Row::new(4, 0);
        let b = Row::new(4, 0);
        assert_ne!(a.gen, b.gen);
        // Equal contents but different generations are still unequal.
        assert_ne!(a, b);
    }

    #[test]
    fn writing_a_shared_row_copies_it() {
        let mut fb = Framebuffer::new(4, 3);
        let snapshot = fb.clone();

        // Every row starts shared between the two framebuffers.
        assert!(Rc::ptr_eq(&fb.rows()[0], &snapshot.rows()[0]));

        fb.mutable_cell(0, 0).append('x');

        // The written row split; the others stayed shared.
        assert!(!Rc::ptr_eq(&fb.rows()[0], &snapshot.rows()[0]));
        assert!(Rc::ptr_eq(&fb.rows()[1], &snapshot.rows()[1]));
        assert!(snapshot.cell(0, 0).unwrap().is_empty());
    }

    #[test]
    fn scrolling_moves_rows_and_blanks_the_rest() {
        let mut fb = Framebuffer::new(4, 3);
        fb.mutable_cell(0, 0).append('a');
        fb.mutable_cell(1, 0).append('b');
        fb.mutable_cell(2, 0).append('c');

        fb.scroll(1);

        assert_eq!(fb.cell(0, 0).unwrap().contents(), "b");
        assert_eq!(fb.cell(1, 0).unwrap().contents(), "c");
        assert!(fb.cell(2, 0).unwrap().is_empty());
    }

    #[test]
    fn insert_and_delete_line_respect_the_scrolling_region() {
        let mut fb = Framebuffer::new(4, 4);
        fb.ds.set_scrolling_region(1, 2);
        fb.mutable_cell(0, 0).append('a');
        fb.mutable_cell(3, 0).append('d');

        // A line inserted outside the region must be ignored entirely.
        fb.insert_line(0, 1);
        assert_eq!(fb.cell(0, 0).unwrap().contents(), "a");
        assert_eq!(fb.cell(3, 0).unwrap().contents(), "d");
    }

    #[test]
    fn cells_shift_on_insert_and_delete() {
        let mut fb = Framebuffer::new(3, 1);
        fb.mutable_cell(0, 0).append('a');
        fb.mutable_cell(0, 1).append('b');

        fb.insert_cell(0, 0);
        assert!(fb.cell(0, 0).unwrap().is_empty());
        assert_eq!(fb.cell(0, 1).unwrap().contents(), "a");

        fb.delete_cell(0, 0);
        assert_eq!(fb.cell(0, 0).unwrap().contents(), "a");
        assert_eq!(fb.cell(0, 1).unwrap().contents(), "b");
        // The row keeps its width.
        assert_eq!(fb.row(0).unwrap().cells.len(), 3);
    }

    #[test]
    fn tabs_start_every_eight_columns() {
        let ds = DrawState::new(80, 24);
        assert_eq!(ds.next_tab(1), 8);
    }

    #[test]
    fn cursor_stays_inside_the_screen() {
        let mut ds = DrawState::new(10, 5);
        ds.move_col(100, false, false);
        assert_eq!(ds.cursor_col(), 9);
        ds.move_row(100, false);
        assert_eq!(ds.cursor_row(), 4);
        ds.move_col(-100, false, false);
        assert_eq!(ds.cursor_col(), 0);
    }

    #[test]
    fn implicit_column_moves_arm_the_wrap() {
        let mut ds = DrawState::new(10, 5);
        ds.move_col(9, false, false);
        assert!(!ds.next_print_will_wrap);
        ds.move_col(1, true, true);
        assert!(ds.next_print_will_wrap);
    }

    #[test]
    fn saving_and_restoring_the_cursor_round_trips() {
        let mut ds = DrawState::new(10, 5);
        ds.move_col(3, false, false);
        ds.move_row(2, false);
        ds.add_rendition(1);
        ds.save_cursor();

        ds.move_col(0, false, false);
        ds.move_row(0, false);
        ds.add_rendition(0);

        ds.restore_cursor();
        assert_eq!(ds.cursor_col(), 3);
        assert_eq!(ds.cursor_row(), 2);
        assert!(ds.renditions().attribute(Attribute::Bold));
    }

    #[test]
    fn origin_mode_confines_the_cursor_to_the_region() {
        let mut ds = DrawState::new(10, 10);
        ds.set_scrolling_region(2, 5);
        ds.origin_mode = true;
        // Row 0 in origin mode is the top of the region.
        ds.move_row(0, false);
        assert_eq!(ds.cursor_row(), 2);
        ds.move_row(100, false);
        assert_eq!(ds.cursor_row(), 5);
    }

    #[test]
    fn resizing_preserves_contents_and_widens_rows() {
        let mut fb = Framebuffer::new(4, 2);
        fb.mutable_cell(0, 0).append('a');
        fb.resize(6, 3);

        assert_eq!(fb.ds.width(), 6);
        assert_eq!(fb.ds.height(), 3);
        assert_eq!(fb.cell(0, 0).unwrap().contents(), "a");
        assert_eq!(fb.row(0).unwrap().cells.len(), 6);
        assert_eq!(fb.rows().len(), 3);
    }

    #[test]
    fn resize_invalidates_an_offscreen_combining_cell() {
        let mut fb = Framebuffer::new(10, 10);
        fb.ds.move_col(9, false, false);
        fb.ds.move_row(9, false);
        assert!(fb.combining_cell().is_some());

        fb.resize(4, 4);
        assert!(fb.combining_cell().is_none());
    }

    #[test]
    fn reset_clears_the_screen_but_not_the_bell() {
        let mut fb = Framebuffer::new(4, 2);
        fb.mutable_cell(0, 0).append('a');
        fb.ring_bell();
        fb.set_window_title(vec!['x']);

        fb.reset();

        assert!(fb.cell(0, 0).unwrap().is_empty());
        assert!(fb.window_title().is_empty());
        assert_eq!(fb.bell_count(), 1);
    }

    #[test]
    fn colour_queries_are_capped_but_still_counted() {
        let mut fb = Framebuffer::new(4, 2);
        let chunk = "x".repeat(1000);
        for _ in 0..10 {
            fb.add_color_query(&chunk);
        }
        assert!(fb.color_queries().len() <= MAXIMUM_COLOR_QUERIES);
        // The sequence number counts every query, including the dropped ones.
        assert_eq!(fb.color_query_seq(), 10);
    }

    #[test]
    fn prefixing_the_title_keeps_icon_name_in_step() {
        let mut fb = Framebuffer::new(4, 2);
        fb.set_window_title(vec!['a']);
        fb.set_icon_name(vec!['a']);
        fb.prefix_window_title(&['[', ']']);
        assert_eq!(fb.window_title(), &['[', ']', 'a']);
        assert_eq!(fb.icon_name(), &['[', ']', 'a']);

        // When they already differ, only the title changes.
        let mut fb = Framebuffer::new(4, 2);
        fb.set_window_title(vec!['a']);
        fb.set_icon_name(vec!['b']);
        fb.prefix_window_title(&['!']);
        assert_eq!(fb.window_title(), &['!', 'a']);
        assert_eq!(fb.icon_name(), &['b']);
    }

    #[test]
    fn framebuffers_compare_equal_after_a_clone() {
        let mut fb = Framebuffer::new(8, 4);
        fb.mutable_cell(1, 1).append('z');
        let copy = fb.clone();
        assert_eq!(fb, copy);

        let mut other = fb.clone();
        other.mutable_cell(1, 1).append('!');
        assert_ne!(fb, other);
    }
}
