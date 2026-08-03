//! The X.364 escape-sequence state machine.
//!
//! Transliterated from `third_party/mosh/src/terminal/parserstate.cc`. The C++ models each state as a
//! class in a hierarchy and each action as a heap-allocated object; here both are enums,
//! which is the same machine without the allocation. State names and the order of rules
//! within each state are kept identical to the C++ so the two can be read side by side.

use mosh_sys::{Decoded, MbDecoder};

/// What the parser tells the emulator to do.
///
/// Mirrors the `Parser::Action` hierarchy. `ch` is absent for actions raised by entering
/// or leaving a state, which carry no character of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub kind: Kind,
    pub ch: Option<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ignore,
    Print,
    Execute,
    Clear,
    Collect,
    Param,
    EscDispatch,
    CsiDispatch,
    Hook,
    Put,
    Unhook,
    OscStart,
    OscPut,
    OscEnd,
}

impl Action {
    fn new(kind: Kind, ch: Option<char>) -> Self {
        Action { kind, ch }
    }

    fn is_ignore(&self) -> bool {
        self.kind == Kind::Ignore
    }
}

impl Kind {
    /// The C++ spelling of this action, so the two implementations can be diffed.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ignore => "Ignore",
            Kind::Print => "Print",
            Kind::Execute => "Execute",
            Kind::Clear => "Clear",
            Kind::Collect => "Collect",
            Kind::Param => "Param",
            Kind::EscDispatch => "Esc_Dispatch",
            Kind::CsiDispatch => "CSI_Dispatch",
            Kind::Hook => "Hook",
            Kind::Put => "Put",
            Kind::Unhook => "Unhook",
            Kind::OscStart => "OSC_Start",
            Kind::OscPut => "OSC_Put",
            Kind::OscEnd => "OSC_End",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

/// An action to raise and, optionally, a state to move to.
struct Transition {
    kind: Kind,
    next: Option<State>,
}

impl Transition {
    /// Raise an action and stay put.
    fn act(kind: Kind) -> Self {
        Transition { kind, next: None }
    }

    /// Raise an action and move.
    fn act_to(kind: Kind, next: State) -> Self {
        Transition {
            kind,
            next: Some(next),
        }
    }

    /// Move without raising an action.
    fn to(next: State) -> Self {
        Transition {
            kind: Kind::Ignore,
            next: Some(next),
        }
    }

    /// Neither act nor move.
    fn nothing() -> Self {
        Transition {
            kind: Kind::Ignore,
            next: None,
        }
    }
}

fn c0_prime(ch: u32) -> bool {
    ch <= 0x17 || ch == 0x19 || (0x1c..=0x1f).contains(&ch)
}

fn glgr(ch: u32) -> bool {
    (0x20..=0x7f).contains(&ch)   // GL area
        || (0xa0..=0xff).contains(&ch) // GR area
}

/// Transitions that apply from any state, checked before the per-state rules.
fn anywhere_rule(ch: u32) -> Option<Transition> {
    if ch == 0x18
        || ch == 0x1a
        || (0x80..=0x8f).contains(&ch)
        || (0x91..=0x97).contains(&ch)
        || ch == 0x99
        || ch == 0x9a
    {
        Some(Transition::act_to(Kind::Execute, State::Ground))
    } else if ch == 0x9c {
        Some(Transition::to(State::Ground))
    } else if ch == 0x1b {
        Some(Transition::to(State::Escape))
    } else if ch == 0x98 || ch == 0x9e || ch == 0x9f {
        Some(Transition::to(State::SosPmApcString))
    } else if ch == 0x90 {
        Some(Transition::to(State::DcsEntry))
    } else if ch == 0x9d {
        Some(Transition::to(State::OscString))
    } else if ch == 0x9b {
        Some(Transition::to(State::CsiEntry))
    } else {
        None
    }
}

impl State {
    /// The action raised on entering this state.
    fn enter(self) -> Kind {
        match self {
            State::Escape | State::CsiEntry | State::DcsEntry => Kind::Clear,
            State::DcsPassthrough => Kind::Hook,
            State::OscString => Kind::OscStart,
            _ => Kind::Ignore,
        }
    }

    /// The action raised on leaving this state.
    fn exit(self) -> Kind {
        match self {
            State::DcsPassthrough => Kind::Unhook,
            State::OscString => Kind::OscEnd,
            _ => Kind::Ignore,
        }
    }

    fn input_state_rule(self, ch: u32) -> Transition {
        match self {
            State::Ground => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if glgr(ch) {
                    Transition::act(Kind::Print)
                } else {
                    Transition::nothing()
                }
            }
            State::Escape => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::EscapeIntermediate)
                } else if (0x30..=0x4f).contains(&ch)
                    || (0x51..=0x57).contains(&ch)
                    || ch == 0x59
                    || ch == 0x5a
                    || ch == 0x5c
                    || (0x60..=0x7e).contains(&ch)
                {
                    Transition::act_to(Kind::EscDispatch, State::Ground)
                } else if ch == 0x5b {
                    Transition::to(State::CsiEntry)
                } else if ch == 0x5d {
                    Transition::to(State::OscString)
                } else if ch == 0x50 {
                    Transition::to(State::DcsEntry)
                } else if ch == 0x58 || ch == 0x5e || ch == 0x5f {
                    Transition::to(State::SosPmApcString)
                } else {
                    Transition::nothing()
                }
            }
            State::EscapeIntermediate => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act(Kind::Collect)
                } else if (0x30..=0x7e).contains(&ch) {
                    Transition::act_to(Kind::EscDispatch, State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::CsiEntry => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::act_to(Kind::CsiDispatch, State::Ground)
                } else if (0x30..=0x39).contains(&ch) || ch == 0x3b {
                    Transition::act_to(Kind::Param, State::CsiParam)
                } else if (0x3c..=0x3f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::CsiParam)
                } else if ch == 0x3a {
                    Transition::to(State::CsiIgnore)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::CsiIntermediate)
                } else {
                    Transition::nothing()
                }
            }
            State::CsiParam => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x30..=0x39).contains(&ch) || ch == 0x3b {
                    Transition::act(Kind::Param)
                } else if ch == 0x3a || (0x3c..=0x3f).contains(&ch) {
                    Transition::to(State::CsiIgnore)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::CsiIntermediate)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::act_to(Kind::CsiDispatch, State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::CsiIntermediate => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act(Kind::Collect)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::act_to(Kind::CsiDispatch, State::Ground)
                } else if (0x30..=0x3f).contains(&ch) {
                    Transition::to(State::CsiIgnore)
                } else {
                    Transition::nothing()
                }
            }
            State::CsiIgnore => {
                if c0_prime(ch) {
                    Transition::act(Kind::Execute)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::to(State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::DcsEntry => {
                if (0x20..=0x2f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::DcsIntermediate)
                } else if ch == 0x3a {
                    Transition::to(State::DcsIgnore)
                } else if (0x30..=0x39).contains(&ch) || ch == 0x3b {
                    Transition::act_to(Kind::Param, State::DcsParam)
                } else if (0x3c..=0x3f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::DcsParam)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::to(State::DcsPassthrough)
                } else {
                    Transition::nothing()
                }
            }
            State::DcsParam => {
                if (0x30..=0x39).contains(&ch) || ch == 0x3b {
                    Transition::act(Kind::Param)
                } else if ch == 0x3a || (0x3c..=0x3f).contains(&ch) {
                    Transition::to(State::DcsIgnore)
                } else if (0x20..=0x2f).contains(&ch) {
                    Transition::act_to(Kind::Collect, State::DcsIntermediate)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::to(State::DcsPassthrough)
                } else {
                    Transition::nothing()
                }
            }
            State::DcsIntermediate => {
                if (0x20..=0x2f).contains(&ch) {
                    Transition::act(Kind::Collect)
                } else if (0x40..=0x7e).contains(&ch) {
                    Transition::to(State::DcsPassthrough)
                } else if (0x30..=0x3f).contains(&ch) {
                    Transition::to(State::DcsIgnore)
                } else {
                    Transition::nothing()
                }
            }
            State::DcsPassthrough => {
                if c0_prime(ch) || (0x20..=0x7e).contains(&ch) {
                    Transition::act(Kind::Put)
                } else if ch == 0x9c {
                    Transition::to(State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::DcsIgnore => {
                if ch == 0x9c {
                    Transition::to(State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::OscString => {
                if (0x20..=0x7f).contains(&ch) {
                    Transition::act(Kind::OscPut)
                } else if ch == 0x9c || ch == 0x07 {
                    // 0x07 is the xterm non-ANSI variant of the terminator.
                    Transition::to(State::Ground)
                } else {
                    Transition::nothing()
                }
            }
            State::SosPmApcString => {
                if ch == 0x9c {
                    Transition::to(State::Ground)
                } else {
                    Transition::nothing()
                }
            }
        }
    }

    fn input(self, ch: char) -> Transition {
        let cp = ch as u32;
        if let Some(anywhere) = anywhere_rule(cp) {
            return anywhere;
        }
        // Codepoints at or above 0xA0 are parsed as though they were 'A', so that any
        // printable character drives the machine the same way.
        self.input_state_rule(if cp >= 0xa0 { 0x41 } else { cp })
    }
}

/// The escape-sequence state machine over decoded characters.
#[derive(Debug, Clone, Default)]
pub struct Parser {
    state: State,
}

impl Parser {
    pub fn new() -> Self {
        Parser::default()
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Feed one character, appending whatever actions it raises.
    pub fn input(&mut self, ch: char, out: &mut Vec<Action>) {
        let tx = self.state.input(ch);

        // Leaving a state raises its exit action before the transition's own action.
        if tx.next.is_some() {
            push_unless_ignore(out, Action::new(self.state.exit(), None));
        }

        push_unless_ignore(out, Action::new(tx.kind, Some(ch)));

        if let Some(next) = tx.next {
            push_unless_ignore(out, Action::new(next.enter(), None));
            self.state = next;
        }
    }
}

fn push_unless_ignore(out: &mut Vec<Action>, action: Action) {
    if !action.is_ignore() {
        out.push(action);
    }
}

/// Largest number of bytes we will hold while waiting for a character to complete.
const BUF_SIZE: usize = 8;

/// Decodes bytes into characters, then drives the state machine.
///
/// Follows Unicode 6.0 section 3.9 on replacement characters, which is why a bad
/// sequence is retried from its last byte rather than discarded whole.
#[derive(Debug, Clone)]
pub struct Utf8Parser {
    parser: Parser,
    buf: Vec<u8>,
}

impl Default for Utf8Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Utf8Parser {
    pub fn new() -> Self {
        Utf8Parser {
            parser: Parser::new(),
            buf: Vec::with_capacity(BUF_SIZE),
        }
    }

    pub fn state(&self) -> State {
        self.parser.state()
    }

    /// Feed one byte, appending whatever actions it raises.
    pub fn input(&mut self, c: u8, out: &mut Vec<Action>) {
        // Single-byte character? Skip the decoder entirely, as the C++ does.
        if self.buf.is_empty() && c <= 0x7f {
            self.parser.input(c as char, out);
            return;
        }

        self.buf.push(c);

        let mut decoder = MbDecoder::new();
        let mut total_parsed = 0usize;
        let orig_len = self.buf.len();

        while total_parsed != orig_len {
            let decoded = decoder.decode(&self.buf);
            let (ch, parsed) = match decoded {
                Decoded::Nul => {
                    self.buf.clear();
                    ('\0', 1)
                }
                Decoded::Invalid => {
                    // Keep the final byte and retry from it; it may begin a valid
                    // sequence even though what preceded it did not.
                    if self.buf.len() > 1 {
                        let last = self.buf[self.buf.len() - 1];
                        let parsed = self.buf.len() - 1;
                        self.buf.clear();
                        self.buf.push(last);
                        ('\u{fffd}', parsed)
                    } else {
                        self.buf.clear();
                        ('\u{fffd}', 1)
                    }
                }
                Decoded::Incomplete => {
                    // Wait for more bytes rather than guessing.
                    break;
                }
                Decoded::Char(ch, n) => {
                    self.buf.drain(..n);
                    (ch, n)
                }
            };

            // Surrogates are ill-formed in UTF-8 even where the platform decodes them,
            // and must not be echoed to the user's terminal.
            let ch = if (0xd800..=0xdfff).contains(&(ch as u32)) {
                '\u{fffd}'
            } else {
                ch
            };

            self.parser.input(ch, out);
            total_parsed += parsed;
        }

        // A sequence longer than any valid character means the input is malformed;
        // drop it rather than growing without bound.
        if self.buf.len() >= BUF_SIZE {
            self.buf.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(actions: &[Action]) -> Vec<Kind> {
        actions.iter().map(|a| a.kind).collect()
    }

    fn feed(bytes: &[u8]) -> (Vec<Action>, Utf8Parser) {
        let mut p = Utf8Parser::new();
        let mut out = Vec::new();
        for &b in bytes {
            p.input(b, &mut out);
        }
        (out, p)
    }

    #[test]
    fn printable_ascii_prints() {
        let (actions, p) = feed(b"hi");
        assert_eq!(kinds(&actions), vec![Kind::Print, Kind::Print]);
        assert_eq!(actions[0].ch, Some('h'));
        assert_eq!(actions[1].ch, Some('i'));
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn control_characters_execute() {
        let (actions, _) = feed(b"\n");
        assert_eq!(kinds(&actions), vec![Kind::Execute]);
        assert_eq!(actions[0].ch, Some('\n'));
    }

    #[test]
    fn csi_sequence_collects_params_then_dispatches() {
        // ESC [ 1 ; 2 H  -- cursor position
        let (actions, p) = feed(b"\x1b[1;2H");
        assert_eq!(
            kinds(&actions),
            vec![
                Kind::Clear,       // entering Escape
                Kind::Clear,       // entering CSI_Entry
                Kind::Param,       // '1'
                Kind::Param,       // ';'
                Kind::Param,       // '2'
                Kind::CsiDispatch, // 'H'
            ]
        );
        assert_eq!(actions.last().unwrap().ch, Some('H'));
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn escape_dispatch_returns_to_ground() {
        let (actions, p) = feed(b"\x1bM");
        assert_eq!(kinds(&actions), vec![Kind::Clear, Kind::EscDispatch]);
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn osc_brackets_its_payload() {
        // ESC ] 0 ; a BEL -- set window title
        let (actions, p) = feed(b"\x1b]0;a\x07");
        assert_eq!(
            kinds(&actions),
            vec![
                Kind::Clear,    // entering Escape
                Kind::OscStart, // entering OSC_String
                Kind::OscPut,   // '0'
                Kind::OscPut,   // ';'
                Kind::OscPut,   // 'a'
                Kind::OscEnd,   // BEL terminates
            ]
        );
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn csi_ignore_swallows_until_final_byte() {
        // A colon in CSI parameters puts the parser into CSI_Ignore.
        let (actions, p) = feed(b"\x1b[1:2m");
        assert_eq!(p.state(), State::Ground);
        // The dispatch is suppressed; only the parameter before the colon was collected.
        assert!(!kinds(&actions).contains(&Kind::CsiDispatch));
    }

    #[test]
    fn escape_cancels_a_sequence_in_progress() {
        // The anywhere rule sends ESC to the Escape state from anywhere.
        let (_, mut p) = feed(b"\x1b[12");
        assert_eq!(p.state(), State::CsiParam);
        let mut out = Vec::new();
        p.input(0x1b, &mut out);
        assert_eq!(p.state(), State::Escape);
    }

    #[test]
    fn can_and_sub_abort_to_ground() {
        for abort in [0x18u8, 0x1a] {
            let (_, mut p) = feed(b"\x1b[12");
            let mut out = Vec::new();
            p.input(abort, &mut out);
            assert_eq!(p.state(), State::Ground);
            assert_eq!(kinds(&out), vec![Kind::Execute]);
        }
    }

    #[test]
    fn multibyte_characters_decode_to_one_print() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        // U+00E9, two bytes.
        let (actions, _) = feed(&[0xc3, 0xa9]);
        assert_eq!(kinds(&actions), vec![Kind::Print]);
        assert_eq!(actions[0].ch, Some('\u{e9}'));
    }

    #[test]
    fn wide_characters_decode_to_one_print() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        // U+4E00, three bytes.
        let (actions, _) = feed(&[0xe4, 0xb8, 0x80]);
        assert_eq!(kinds(&actions), vec![Kind::Print]);
        assert_eq!(actions[0].ch, Some('\u{4e00}'));
    }

    #[test]
    fn invalid_sequences_become_replacement_characters() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        let (actions, _) = feed(&[0xff]);
        assert_eq!(kinds(&actions), vec![Kind::Print]);
        assert_eq!(actions[0].ch, Some('\u{fffd}'));
    }

    #[test]
    fn a_truncated_sequence_does_not_consume_the_next_character() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        // Lead byte of a 3-byte character, then a plain ASCII 'a'. Per Unicode 6.0
        // section 3.9 the bad prefix yields U+FFFD and 'a' survives.
        let (actions, _) = feed(&[0xe4, b'a']);
        let chars: Vec<Option<char>> = actions.iter().map(|a| a.ch).collect();
        assert!(chars.contains(&Some('a')), "the trailing 'a' was swallowed: {chars:?}");
    }

    #[test]
    fn high_codepoints_drive_the_machine_like_letters() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        // In Ground a high codepoint prints; the 0xA0 folding must not change that.
        let (actions, p) = feed(&[0xe4, 0xb8, 0x80]);
        assert_eq!(kinds(&actions), vec![Kind::Print]);
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn dcs_passthrough_hooks_and_unhooks() {
        mosh_sys::set_native_locale();
        if !mosh_sys::is_utf8_locale() {
            return;
        }
        // ESC P q ST. The terminator is U+009C, which reaches the state machine only
        // after UTF-8 decoding, so it must be fed as its two-byte encoding.
        let (actions, p) = feed(b"\x1bPq\xc2\x9c");
        let k = kinds(&actions);
        assert!(k.contains(&Kind::Hook), "{k:?}");
        assert!(k.contains(&Kind::Unhook), "{k:?}");
        assert_eq!(p.state(), State::Ground);
    }

    #[test]
    fn buffer_cannot_grow_without_bound() {
        mosh_sys::set_native_locale();
        // Repeated continuation bytes are never a valid start; the buffer must not grow.
        let mut p = Utf8Parser::new();
        let mut out = Vec::new();
        for _ in 0..1000 {
            p.input(0xe4, &mut out);
        }
        assert!(p.buf.len() < BUF_SIZE);
    }
}
