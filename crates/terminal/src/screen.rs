//! Server-side terminal state for one pane.
//!
//! One engine (alacritty_terminal) owns the truth about what is on screen.
//! Clients never get raw replay: a late joiner receives [`Screen::snapshot_ansi`]
//! (an escape-sequence program that repaints the grid) and then the live stream.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use serde::{Deserialize, Serialize};

/// Things the terminal asked for while parsing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenEvent {
    /// Bytes the terminal must write back to the PTY (device status replies etc).
    Reply(Vec<u8>),
    Title(String),
    Bell,
}

#[derive(Clone, Default)]
struct Listener {
    events: Arc<Mutex<Vec<ScreenEvent>>>,
    size: Arc<Mutex<(u16, u16)>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let ev = match event {
            Event::PtyWrite(s) => ScreenEvent::Reply(s.into_bytes()),
            Event::Title(t) => ScreenEvent::Title(t),
            Event::ResetTitle => ScreenEvent::Title(String::new()),
            Event::Bell => ScreenEvent::Bell,
            Event::TextAreaSizeRequest(fmt) => {
                let (cols, rows) = *self.size.lock().unwrap();
                let ws = alacritty_terminal::event::WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 8,
                    cell_height: 16,
                };
                ScreenEvent::Reply(fmt(ws).into_bytes())
            }
            // Colour and clipboard queries are answered by the real client, not here.
            _ => return,
        };
        self.events.lock().unwrap().push(ev);
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Plain-data description of a screen, enough to describe it to a client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenInfo {
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub alt_screen: bool,
    pub title: String,
}

/// Follows the byte stream beside the engine, for two things the engine does
/// not expose:
/// - the scroll region (`CSI t ; b r` sets it; a reset or resize clears it);
/// - the bytes of a token that has started but not finished (half an escape
///   sequence, half a UTF-8 character). A snapshot taken at that instant ends
///   with those bytes, so the client's parser is left in the same state and
///   the rest of the token, arriving on the live stream, completes it.
#[derive(Default)]
struct StreamTracker {
    state: St,
    pending: Vec<u8>,
    utf8_left: u8,
    region: Option<(u16, u16)>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum St {
    #[default]
    Ground,
    Esc,
    EscInter,
    Csi,
    Str,
    StrEsc,
}

impl StreamTracker {
    fn feed(&mut self, bytes: &[u8], rows: u16) {
        for &b in bytes {
            self.byte(b, rows);
        }
    }

    fn ground(&mut self) {
        self.state = St::Ground;
        self.pending.clear();
        self.utf8_left = 0;
    }

    fn byte(&mut self, b: u8, rows: u16) {
        match self.state {
            St::Ground => {
                if b == 0x1b {
                    self.pending.clear();
                    self.pending.push(b);
                    self.utf8_left = 0;
                    self.state = St::Esc;
                } else if self.utf8_left > 0 && (0x80..0xc0).contains(&b) {
                    self.pending.push(b);
                    self.utf8_left -= 1;
                    if self.utf8_left == 0 {
                        self.pending.clear();
                    }
                } else {
                    self.pending.clear();
                    self.utf8_left = match b {
                        0xc0..=0xdf => 1,
                        0xe0..=0xef => 2,
                        0xf0..=0xf7 => 3,
                        _ => 0,
                    };
                    if self.utf8_left > 0 {
                        self.pending.push(b);
                    }
                }
            }
            St::Esc => match b {
                b'[' => {
                    self.pending.push(b);
                    self.state = St::Csi;
                }
                b']' | b'P' | b'^' | b'_' | b'X' => {
                    self.pending.push(b);
                    self.state = St::Str;
                }
                0x20..=0x2f => {
                    self.pending.push(b);
                    self.state = St::EscInter;
                }
                0x1b => {}
                b'c' => {
                    self.region = None;
                    self.ground();
                }
                _ => self.ground(),
            },
            St::EscInter => match b {
                0x20..=0x2f => self.pending.push(b),
                0x1b => {
                    self.pending.clear();
                    self.pending.push(b);
                    self.state = St::Esc;
                }
                _ => self.ground(),
            },
            St::Csi => match b {
                0x20..=0x3f => {
                    if self.pending.len() < 64 {
                        self.pending.push(b);
                    } else {
                        self.ground();
                    }
                }
                0x1b => {
                    self.pending.clear();
                    self.pending.push(b);
                    self.state = St::Esc;
                }
                final_byte => {
                    let params = &self.pending[2..];
                    match final_byte {
                        b'r' if params.iter().all(|c| c.is_ascii_digit() || *c == b';') => {
                            let text = String::from_utf8_lossy(params);
                            let mut it = text.split(';');
                            let top = it.next().and_then(|n| n.parse::<u16>().ok()).unwrap_or(1).max(1);
                            let bottom = it.next().and_then(|n| n.parse::<u16>().ok()).filter(|b| *b > 0).unwrap_or(rows).min(rows);
                            self.region = (top < bottom && !(top == 1 && bottom == rows)).then_some((top, bottom));
                        }
                        // Soft reset.
                        b'p' if params == b"!" => self.region = None,
                        _ => {}
                    }
                    self.ground();
                }
            },
            St::Str => match b {
                0x07 => self.ground(),
                0x1b => {
                    self.pending.push(b);
                    self.state = St::StrEsc;
                }
                _ => {
                    if self.pending.len() < 8192 {
                        self.pending.push(b);
                    } else {
                        self.ground();
                    }
                }
            },
            St::StrEsc => {
                if b == b'\\' {
                    self.ground();
                } else {
                    // The string was cut off by a new escape sequence.
                    self.pending.clear();
                    self.pending.push(0x1b);
                    self.state = St::Esc;
                    self.byte(b, rows);
                }
            }
        }
    }
}

pub struct Screen {
    region: StreamTracker,
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    cols: u16,
    rows: u16,
    title: String,
}

pub const DEFAULT_HISTORY: usize = 5000;

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, DEFAULT_HISTORY)
    }

    pub fn with_history(cols: u16, rows: u16, history: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let listener = Listener::default();
        *listener.size.lock().unwrap() = (cols, rows);
        let config = Config { scrolling_history: history, ..Config::default() };
        let term = Term::new(
            config,
            &Size { cols: cols as usize, rows: rows as usize },
            listener.clone(),
        );
        Screen { region: StreamTracker::default(), term, parser: Processor::new(), listener, cols, rows, title: String::new() }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Feed PTY output. Returns what the terminal asked for.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ScreenEvent> {
        self.parser.advance(&mut self.term, bytes);
        self.region.feed(bytes, self.rows);
        let events = std::mem::take(&mut *self.listener.events.lock().unwrap());
        for ev in &events {
            if let ScreenEvent::Title(t) = ev {
                self.title = t.clone();
            }
        }
        events
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        *self.listener.size.lock().unwrap() = (cols, rows);
        self.region.region = None;
        self.term.resize(Size { cols: cols as usize, rows: rows as usize });
    }

    pub fn info(&self) -> ScreenInfo {
        let mode = *self.term.mode();
        let cursor = self.term.grid().cursor.point;
        ScreenInfo {
            cols: self.cols,
            rows: self.rows,
            cursor_row: cursor.line.0.max(0) as u16,
            cursor_col: cursor.column.0 as u16,
            cursor_visible: mode.contains(TermMode::SHOW_CURSOR),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            title: self.title.clone(),
        }
    }

    fn line_text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let mut s = String::with_capacity(self.cols as usize);
        for col in 0..self.cols as usize {
            let cell = &grid[line][Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(if cell.c == '\0' { ' ' } else { cell.c });
            if let Some(zw) = cell.zerowidth() {
                s.extend(zw.iter());
            }
        }
        let trimmed = s.trim_end().len();
        s.truncate(trimmed);
        s
    }

    /// The visible grid as text, one string per row, right-trimmed.
    pub fn lines(&self) -> Vec<String> {
        (0..self.rows as i32).map(|i| self.line_text(Line(i))).collect()
    }

    /// The visible grid as one string. This is what golden tests compare.
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// Scrollback plus the visible grid as text, oldest line first.
    pub fn full_text(&self, max_history: usize) -> Vec<String> {
        let hist = self.term.grid().history_size().min(max_history) as i32;
        (-hist..self.rows as i32).map(|i| self.line_text(Line(i))).collect()
    }

    /// Number of scrollback lines currently held.
    pub fn history_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// An escape-sequence program that reproduces this screen on a fresh
    /// terminal of the same size: scrollback (up to `max_history` lines),
    /// the grid with attributes, cursor, modes and title.
    ///
    /// While a full-screen program is open, both screens are carried: the
    /// shell screen underneath first, then the alternate screen on top, so
    /// leaving the program later shows what was there before.
    pub fn snapshot_ansi(&mut self, max_history: usize) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(self.cols as usize * self.rows as usize * 2);
        let mode = *self.term.mode();
        // Full reset so a resync never inherits stale modes.
        out.extend_from_slice(b"\x1bc");

        if mode.contains(TermMode::ALT_SCREEN) {
            let alt = self.paint_active(true, 0, true);
            // The engine only exposes the active grid: step out to read the
            // primary screen, step back in, and put the alternate screen back
            // (entering it clears it).
            self.term.swap_alt();
            let primary = self.paint_active(false, max_history, false);
            self.term.swap_alt();
            let mut restore: Processor = Processor::new();
            restore.advance(&mut self.term, &alt);
            self.listener.events.lock().unwrap().clear();

            out.extend_from_slice(&primary);
            out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
            out.extend_from_slice(&alt);
        } else {
            out.extend_from_slice(&self.paint_active(false, max_history, true));
        }

        for (flag, seq) in [
            (TermMode::APP_CURSOR, "\x1b[?1h"),
            (TermMode::APP_KEYPAD, "\x1b="),
            (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h"),
            (TermMode::MOUSE_DRAG, "\x1b[?1002h"),
            (TermMode::MOUSE_MOTION, "\x1b[?1003h"),
            (TermMode::SGR_MOUSE, "\x1b[?1006h"),
            (TermMode::UTF8_MOUSE, "\x1b[?1005h"),
            (TermMode::FOCUS_IN_OUT, "\x1b[?1004h"),
            (TermMode::BRACKETED_PASTE, "\x1b[?2004h"),
            (TermMode::ALTERNATE_SCROLL, "\x1b[?1007h"),
            (TermMode::ORIGIN, "\x1b[?6h"),
            (TermMode::INSERT, "\x1b[4h"),
        ] {
            if mode.contains(flag) {
                push_str(&mut out, seq);
            }
        }
        if !mode.contains(TermMode::LINE_WRAP) {
            push_str(&mut out, "\x1b[?7l");
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            push_str(&mut out, "\x1b[?25l");
        }
        if !self.title.is_empty() {
            let clean: String = self.title.chars().filter(|c| !c.is_control()).collect();
            push_str(&mut out, &format!("\x1b]0;{clean}\x07"));
        }
        // Last: whatever token the stream was in the middle of.
        out.extend_from_slice(&self.region.pending);
        out
    }

    /// Paint the active grid, then set the pen and cursor the application left.
    /// `absolute` addresses each row (alternate screen); otherwise rows flow
    /// from the top so scrollback lands in the client's scrollback.
    fn paint_active(&self, absolute: bool, max_history: usize, with_region: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let grid = self.term.grid();
        let mut pen = Pen::default();
        if absolute {
            for row in 0..self.rows as i32 {
                push_str(&mut out, &format!("\x1b[{};1H", row + 1));
                self.paint_row(&mut out, Line(row), &mut pen);
            }
        } else {
            let hist = grid.history_size().min(max_history) as i32;
            let last = self.rows as i32 - 1;
            for row in -hist..=last {
                let wrapped = self.paint_row(&mut out, Line(row), &mut pen);
                if row != last && !wrapped {
                    if pen != Pen::default() {
                        out.extend_from_slice(b"\x1b[0m");
                        pen = Pen::default();
                    }
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
        // The scroll region goes in after the rows (painting inside one would
        // scroll it) and before the cursor (setting it homes the cursor).
        if let (Some((top, bottom)), true) = (self.region.region, with_region) {
            push_str(&mut out, &format!("\x1b[{top};{bottom}r"));
        }
        let cursor = grid.cursor.point;
        push_str(&mut out, &format!("\x1b[{};{}H", cursor.line.0.max(0) + 1, cursor.column.0 + 1));
        if grid.cursor.input_needs_wrap {
            // The cursor sits past a full row, waiting to wrap. Addressing it
            // loses that, so rewrite the last cell to get the state back.
            let cell = &grid[cursor.line][cursor.column];
            if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                Pen::of(cell).emit(&mut out);
                let mut buf = [0u8; 4];
                let c = if cell.c == '\0' { ' ' } else { cell.c };
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        // Pen the application left active, so later output continues correctly.
        Pen::of(&grid.cursor.template).emit(&mut out);
        out
    }

    /// Paint one row at the current cursor position. Returns true when the row
    /// soft-wraps into the next one (so no newline must follow it).
    fn paint_row(&self, out: &mut Vec<u8>, line: Line, pen: &mut Pen) -> bool {
        let grid = self.term.grid();
        let row = &grid[line];
        let cols = self.cols as usize;
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);

        // Last cell that is not a default blank; a wrapped row is painted in full.
        let mut end = cols;
        if !wrapped {
            while end > 0 && is_blank(&row[Column(end - 1)]) {
                end -= 1;
            }
        }
        let mut buf = [0u8; 4];
        for col in 0..end {
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let want = Pen::of(cell);
            if want != *pen {
                want.emit(out);
                *pen = want;
            }
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            if let Some(zw) = cell.zerowidth() {
                for z in zw {
                    out.extend_from_slice(z.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        wrapped
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

fn is_blank(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\0')
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
        && cell.zerowidth().is_none()
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Default for Pen {
    fn default() -> Self {
        Pen {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }
}

const STYLE_FLAGS: Flags = Flags::INVERSE
    .union(Flags::BOLD)
    .union(Flags::ITALIC)
    .union(Flags::DIM)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT)
    .union(Flags::ALL_UNDERLINES);

impl Pen {
    fn of(cell: &Cell) -> Pen {
        Pen { fg: cell.fg, bg: cell.bg, flags: cell.flags & STYLE_FLAGS }
    }

    fn emit(&self, out: &mut Vec<u8>) {
        let mut s = String::from("\x1b[0");
        let f = self.flags;
        if f.contains(Flags::BOLD) {
            s.push_str(";1");
        }
        if f.contains(Flags::DIM) {
            s.push_str(";2");
        }
        if f.contains(Flags::ITALIC) {
            s.push_str(";3");
        }
        if f.contains(Flags::UNDERLINE) {
            s.push_str(";4");
        }
        if f.contains(Flags::DOUBLE_UNDERLINE) {
            s.push_str(";21");
        }
        if f.contains(Flags::UNDERCURL) {
            s.push_str(";4:3");
        }
        if f.contains(Flags::DOTTED_UNDERLINE) {
            s.push_str(";4:4");
        }
        if f.contains(Flags::DASHED_UNDERLINE) {
            s.push_str(";4:5");
        }
        if f.contains(Flags::INVERSE) {
            s.push_str(";7");
        }
        if f.contains(Flags::HIDDEN) {
            s.push_str(";8");
        }
        if f.contains(Flags::STRIKEOUT) {
            s.push_str(";9");
        }
        color_sgr(&mut s, self.fg, true);
        color_sgr(&mut s, self.bg, false);
        s.push('m');
        out.extend_from_slice(s.as_bytes());
    }
}

fn color_sgr(s: &mut String, color: Color, fg: bool) {
    use std::fmt::Write;
    match color {
        Color::Named(n) => {
            let idx = n as usize;
            match idx {
                0..=7 => {
                    let _ = write!(s, ";{}", if fg { 30 + idx } else { 40 + idx });
                }
                8..=15 => {
                    let _ = write!(s, ";{}", if fg { 90 + idx - 8 } else { 100 + idx - 8 });
                }
                // Foreground / Background / dim variants: the reset already covers them.
                _ => {}
            }
        }
        Color::Indexed(i) => {
            let _ = write!(s, ";{};5;{}", if fg { 38 } else { 48 }, i);
        }
        Color::Spec(rgb) => {
            let _ = write!(s, ";{};2;{};{};{}", if fg { 38 } else { 48 }, rgb.r, rgb.g, rgb.b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_lands_on_the_grid() {
        let mut s = Screen::new(20, 4);
        s.feed(b"hello\r\nworld");
        assert_eq!(s.text(), "hello\nworld\n\n");
        let info = s.info();
        assert_eq!((info.cursor_row, info.cursor_col), (1, 5));
    }

    #[test]
    fn device_status_query_gets_a_reply() {
        let mut s = Screen::new(20, 4);
        let ev = s.feed(b"ab\x1b[6n");
        assert_eq!(ev, vec![ScreenEvent::Reply(b"\x1b[1;3R".to_vec())]);
    }

    #[test]
    fn title_is_tracked() {
        let mut s = Screen::new(20, 4);
        s.feed(b"\x1b]0;my title\x07");
        assert_eq!(s.title(), "my title");
    }

    /// The core contract for late joiners and handoff: replaying a snapshot on a
    /// fresh screen gives the same grid, cursor and modes.
    fn assert_roundtrip(input: &[u8], cols: u16, rows: u16) {
        let mut a = Screen::new(cols, rows);
        a.feed(input);
        let before = (a.full_text(1000), a.info());
        let snap = a.snapshot_ansi(1000);
        assert_eq!(before, (a.full_text(1000), a.info()), "taking a snapshot changed the screen");
        let mut b = Screen::new(cols, rows);
        b.feed(&snap);
        assert_eq!(a.full_text(1000), b.full_text(1000), "grid differs");
        assert_eq!(a.info(), b.info(), "cursor or modes differ");
        // A snapshot of the copy must be byte-identical: attributes survived too.
        assert_eq!(snap, b.snapshot_ansi(1000), "attributes differ");
    }

    #[test]
    fn snapshot_roundtrip_plain() {
        assert_roundtrip(b"one\r\ntwo\r\nthree", 20, 5);
    }

    #[test]
    fn snapshot_roundtrip_colours_and_styles() {
        assert_roundtrip(
            b"\x1b[1;31mred bold\x1b[0m plain \x1b[38;5;200;48;2;1;2;3mfancy\x1b[0m\r\n\x1b[4;7munder inverse",
            40,
            5,
        );
    }

    #[test]
    fn snapshot_roundtrip_scrollback_and_wrap() {
        let mut input = Vec::new();
        for i in 0..30 {
            input.extend_from_slice(format!("line {i} {}\r\n", "x".repeat(i * 2)).as_bytes());
        }
        assert_roundtrip(&input, 24, 6);
    }

    #[test]
    fn snapshot_roundtrip_alt_screen_and_modes() {
        assert_roundtrip(
            b"shell$ vim\r\n\x1b[?1049h\x1b[2J\x1b[3;5Hin alt\x1b[?25l\x1b[?2004h\x1b[?1000h\x1b[?1006h",
            30,
            8,
        );
    }

    #[test]
    fn snapshot_roundtrip_wide_chars() {
        assert_roundtrip("日本語 ok \u{1F600} e\u{301}".as_bytes(), 30, 4);
    }

    #[test]
    fn resize_keeps_content() {
        let mut s = Screen::new(20, 4);
        s.feed(b"keep me");
        s.resize(40, 10);
        assert_eq!(s.lines()[0], "keep me");
        assert_eq!((s.cols(), s.rows()), (40, 10));
    }
}
