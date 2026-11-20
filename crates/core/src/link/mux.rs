//! Many streams over one ordered byte stream: the frames of a link, and
//! the rules both ends keep. No reading or writing happens here: bytes
//! that arrived are fed in, bytes to send are taken out, and what happened
//! is read as events (`conn.rs` does the reading and writing).
//!
//! What goes over the byte stream:
//!
//!   eight bytes, once:  "spdklnk\n"
//!   then frames:        stream id (4 bytes) | kind (1) | flags (1) |
//!                       length (4) | that many bytes
//!
//! Numbers are big-endian. The first frame of each end is its hello
//! (stream 0, kind 0): a small JSON object that says `proto`, and whatever
//! else that end has to say about itself. Two ends whose `proto` differ
//! stop there, with a sentence.
//!
//! Flags: OPEN begins a stream of the frame's kind; FIN says this end will
//! send no more on it; RESET ends it both ways at once; CREDIT carries no
//! data but four bytes, the number of further bytes the other end may send
//! on it.
//!
//! Each stream has a window of its own. An end sends no more on a stream
//! than the other has allowed, and allows more only as what arrived is
//! used. So a stream whose reader has stopped holds at most one window in
//! memory and then stops being sent, while every other stream goes on; and
//! what is sent is taken a frame from each stream in turn, so a stream
//! with megabytes waiting delays another's few bytes by one frame.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

/// What begins every link, before the first frame.
pub const MAGIC: &[u8; 8] = b"spdklnk\n";
/// The version of these rules. Both ends must say the same.
pub const PROTO: u64 = 1;
pub const HEADER: usize = 10;
/// The longest frame taken. A header that says more ends the link.
pub const MAX_FRAME: usize = 64 * 1024;
/// The longest frame sent.
pub const SEND_FRAME: usize = 16 * 1024;
/// How much one stream may have on its way, unread, in one direction.
pub const WINDOW: u32 = 256 * 1024;
/// How many streams a link carries at once.
pub const MAX_STREAMS: usize = 256;
/// The longest hello.
const MAX_HELLO: usize = 4096;

/// What a stream carries. The number is the frame's kind byte. An end that
/// is offered a kind it does not know refuses that stream and nothing
/// else, so kinds can be added: 4, 5 and 6 are kept for a station's call,
/// a pane's socket and a file.
pub mod kind {
    /// The link's own frames (the hello). Never a stream.
    pub const LINK: u8 = 0;
    /// One TCP connection from a browser to the far station's web listener.
    pub const WEB: u8 = 1;
    /// The heartbeat: what is sent comes back.
    pub const PING: u8 = 2;
    /// One question about signing a browser in, and its answer.
    pub const SIGN: u8 = 3;

    pub fn name(kind: u8) -> &'static str {
        match kind {
            WEB => "web",
            PING => "ping",
            SIGN => "sign",
            4 => "call",
            5 => "pane",
            6 => "file",
            _ => "unknown",
        }
    }
}

const OPEN: u8 = 1;
const FIN: u8 = 2;
const RESET: u8 = 4;
const CREDIT: u8 = 8;

/// Why a link cannot go on. Each is said to the owner as it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxError {
    /// What arrived first is not a link's beginning. Holds the first of it,
    /// cleaned.
    NotALink(String),
    /// The other end keeps another version of these rules.
    Version { theirs: u64, ours: u64 },
    /// A frame's header said it is longer than any frame may be.
    FrameTooLarge(usize),
    /// The byte stream ended in the middle of a frame.
    CutMidFrame,
    /// More arrived on a stream than it was allowed.
    WindowOverrun(u32),
    /// Anything else the other end must never send.
    Protocol(&'static str),
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MuxError::NotALink(first) if first.is_empty() => {
                write!(f, "the other end said nothing a link begins with")
            }
            MuxError::NotALink(first) => write!(
                f,
                "the other end did not begin a link; it said: {first}"
            ),
            MuxError::Version { theirs, ours } => write!(
                f,
                "the other end speaks version {theirs} of the link and this one speaks version {ours}: update the older spandeck"
            ),
            MuxError::FrameTooLarge(n) => write!(
                f,
                "the other end sent a frame of {n} bytes, more than a link takes"
            ),
            MuxError::CutMidFrame => write!(f, "the link was cut in the middle of a frame"),
            MuxError::WindowOverrun(id) => write!(
                f,
                "the other end sent more on stream {id} than it was allowed"
            ),
            MuxError::Protocol(what) => write!(f, "the other end broke the link's rules: {what}"),
        }
    }
}

impl std::error::Error for MuxError {}

/// What happened, in the order it arrived.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The other end's hello.
    Hello(Value),
    /// The other end opened a stream, and it was taken.
    Opened {
        id: u32,
        kind: u8,
    },
    /// The other end tried to open a stream, and it was refused.
    Refused {
        kind: u8,
    },
    Data {
        id: u32,
        bytes: Vec<u8>,
    },
    /// The other end will send no more on this stream.
    Fin {
        id: u32,
    },
    /// The stream is gone, both ways.
    Reset {
        id: u32,
    },
    /// More may be sent on this stream than a moment ago.
    Writable {
        id: u32,
    },
}

struct Stream {
    kind: u8,
    /// How much more this end may hand to `send`.
    send_window: u32,
    /// Taken by `send` and not yet in a frame. Never more than a window.
    out: VecDeque<u8>,
    open_sent: bool,
    fin_asked: bool,
    fin_sent: bool,
    /// How much more the other end may send.
    recv_window: u32,
    /// Used here and not yet allowed again.
    used: u32,
    peer_fin: bool,
    /// In the round of streams with something to send.
    in_round: bool,
}

impl Stream {
    fn new(kind: u8, open_sent: bool) -> Stream {
        Stream {
            kind,
            send_window: WINDOW,
            out: VecDeque::new(),
            open_sent,
            fin_asked: false,
            fin_sent: false,
            recv_window: WINDOW,
            used: 0,
            peer_fin: false,
            in_round: false,
        }
    }

    fn has_frame(&self) -> bool {
        !self.out.is_empty() || !self.open_sent || (self.fin_asked && !self.fin_sent)
    }

    fn done(&self) -> bool {
        self.fin_sent && self.peer_fin
    }
}

pub struct Mux {
    /// The kinds of stream the other end may open here. Empty: none.
    accepts: &'static [u8],
    /// The ids this end gives the streams it opens: odd or even.
    next_id: u32,
    last_peer_id: u32,
    inbuf: Vec<u8>,
    preface_read: bool,
    hello_read: bool,
    streams: HashMap<u32, Stream>,
    round: VecDeque<u32>,
    /// Whole frames that go before any stream's data.
    control: VecDeque<Vec<u8>>,
    events: VecDeque<Event>,
    refused: u64,
}

fn frame(id: u32, kind: u8, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER + payload.len());
    f.extend_from_slice(&id.to_be_bytes());
    f.push(kind);
    f.push(flags);
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// Text from the other end, for a sentence: printable, and short.
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if out.chars().count() >= max {
            break;
        }
        if c.is_control() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

impl Mux {
    /// The end that opens streams and takes none: the station the owner
    /// is at.
    pub fn near() -> Mux {
        Mux::new(&[], 1)
    }

    /// The end that takes the streams the near one opens.
    pub fn far() -> Mux {
        Mux::new(&[kind::WEB, kind::PING, kind::SIGN], 2)
    }

    fn new(accepts: &'static [u8], first_id: u32) -> Mux {
        Mux {
            accepts,
            next_id: first_id,
            last_peer_id: 0,
            inbuf: Vec::new(),
            preface_read: false,
            hello_read: false,
            streams: HashMap::new(),
            round: VecDeque::new(),
            control: VecDeque::new(),
            events: VecDeque::new(),
            refused: 0,
        }
    }

    /// Begin the link from this end: the eight bytes and the hello. `more`
    /// is what this end says of itself; `proto` is added here.
    pub fn hello(&mut self, mut more: Value) {
        more["proto"] = Value::from(PROTO);
        let mut first = MAGIC.to_vec();
        first.extend(frame(0, kind::LINK, 0, more.to_string().as_bytes()));
        self.control.push_front(first);
    }

    /// How many streams the other end tried to open and was refused.
    pub fn refused(&self) -> u64 {
        self.refused
    }

    pub fn streams(&self) -> usize {
        self.streams.len()
    }

    /// Bytes that arrived. An error ends the link: nothing more is fed.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), MuxError> {
        self.inbuf.extend_from_slice(bytes);
        let mut at = 0;
        if !self.preface_read {
            let have = self.inbuf.len().min(MAGIC.len());
            if self.inbuf[..have] != MAGIC[..have] {
                let first = String::from_utf8_lossy(&self.inbuf[..self.inbuf.len().min(200)]);
                return Err(MuxError::NotALink(clean(
                    first.lines().next().unwrap_or(""),
                    120,
                )));
            }
            if have < MAGIC.len() {
                return Ok(());
            }
            self.preface_read = true;
            at = MAGIC.len();
        }
        let done = loop {
            let rest = &self.inbuf[at..];
            if rest.len() < HEADER {
                break Ok(());
            }
            let id = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
            let (kind, flags) = (rest[4], rest[5]);
            let len = u32::from_be_bytes([rest[6], rest[7], rest[8], rest[9]]) as usize;
            // Refused by its header alone: the bytes are never waited for.
            if len > MAX_FRAME {
                break Err(MuxError::FrameTooLarge(len));
            }
            if rest.len() < HEADER + len {
                break Ok(());
            }
            let payload = rest[HEADER..HEADER + len].to_vec();
            at += HEADER + len;
            if let Err(e) = self.take(id, kind, flags, payload) {
                break Err(e);
            }
        };
        self.inbuf.drain(..at);
        done
    }

    /// The byte stream ended. Between frames that is an ordinary ending.
    pub fn eof(&self) -> Result<(), MuxError> {
        match (self.preface_read, self.inbuf.is_empty()) {
            (false, true) => Err(MuxError::NotALink(String::new())),
            (_, false) => Err(MuxError::CutMidFrame),
            (true, true) => Ok(()),
        }
    }

    fn take(&mut self, id: u32, kind: u8, flags: u8, payload: Vec<u8>) -> Result<(), MuxError> {
        if !self.hello_read {
            if id != 0 || kind != kind::LINK {
                return Err(MuxError::Protocol("its first frame was not a hello"));
            }
            if payload.len() > MAX_HELLO {
                return Err(MuxError::Protocol("its hello was too long"));
            }
            let hello: Value = serde_json::from_slice(&payload)
                .map_err(|_| MuxError::Protocol("its hello could not be read"))?;
            let theirs = hello["proto"].as_u64().unwrap_or(0);
            if theirs != PROTO {
                return Err(MuxError::Version {
                    theirs,
                    ours: PROTO,
                });
            }
            self.hello_read = true;
            self.events.push_back(Event::Hello(hello));
            return Ok(());
        }
        // A frame of the link itself that this version does not know: a
        // later version's, and nothing to this one.
        if id == 0 || kind == kind::LINK {
            return Ok(());
        }
        if flags & OPEN != 0 {
            let ours = id % 2 == self.next_id % 2;
            if ours || id <= self.last_peer_id {
                return Err(MuxError::Protocol(
                    "it opened a stream under a number it may not use",
                ));
            }
            self.last_peer_id = id;
            if !self.accepts.contains(&kind) || self.streams.len() >= MAX_STREAMS {
                self.refused += 1;
                self.control.push_back(frame(id, kind, RESET, &[]));
                self.events.push_back(Event::Refused { kind });
                return Ok(());
            }
            self.streams.insert(id, Stream::new(kind, true));
            self.events.push_back(Event::Opened { id, kind });
        }
        // A stream that is gone here: what was already on its way is
        // dropped, and that is no fault of the other end's.
        let Some(s) = self.streams.get_mut(&id) else {
            return Ok(());
        };
        if flags & RESET != 0 {
            self.streams.remove(&id);
            self.events.push_back(Event::Reset { id });
            return Ok(());
        }
        if flags & CREDIT != 0 {
            let Ok(four) = <[u8; 4]>::try_from(payload.as_slice()) else {
                return Err(MuxError::Protocol("a credit was not four bytes"));
            };
            // Never more than a window in all, whatever it says.
            let room = WINDOW - (s.out.len() as u32).min(WINDOW);
            s.send_window = s
                .send_window
                .saturating_add(u32::from_be_bytes(four))
                .min(room);
            self.events.push_back(Event::Writable { id });
            return Ok(());
        }
        if !payload.is_empty() {
            if s.peer_fin {
                return Err(MuxError::Protocol("it sent on a stream it had finished"));
            }
            let n = payload.len() as u32;
            if n > s.recv_window {
                return Err(MuxError::WindowOverrun(id));
            }
            s.recv_window -= n;
            self.events.push_back(Event::Data { id, bytes: payload });
        }
        if flags & FIN != 0 && !s.peer_fin {
            s.peer_fin = true;
            self.events.push_back(Event::Fin { id });
            if s.done() {
                self.streams.remove(&id);
            }
        }
        Ok(())
    }

    pub fn event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Open a stream. `None`: the link already carries as many as it may.
    pub fn open(&mut self, kind: u8) -> Option<u32> {
        if self.streams.len() >= MAX_STREAMS {
            return None;
        }
        let id = self.next_id;
        self.next_id += 2;
        self.streams.insert(id, Stream::new(kind, false));
        self.queue(id);
        Some(id)
    }

    fn queue(&mut self, id: u32) {
        if let Some(s) = self.streams.get_mut(&id) {
            if !s.in_round && s.has_frame() {
                s.in_round = true;
                self.round.push_back(id);
            }
        }
    }

    /// Is this stream still there?
    pub fn has(&self, id: u32) -> bool {
        self.streams.contains_key(&id)
    }

    /// Take bytes to send on a stream: as many as its window allows, which
    /// may be none. Says how many were taken; the rest is offered again
    /// after `Event::Writable`.
    pub fn send(&mut self, id: u32, bytes: &[u8]) -> usize {
        let Some(s) = self.streams.get_mut(&id) else {
            return 0;
        };
        if s.fin_asked {
            return 0;
        }
        let n = bytes.len().min(s.send_window as usize);
        s.out.extend(&bytes[..n]);
        s.send_window -= n as u32;
        self.queue(id);
        n
    }

    /// This end will send no more on the stream (what `send` took is sent
    /// first).
    pub fn fin(&mut self, id: u32) {
        if let Some(s) = self.streams.get_mut(&id) {
            s.fin_asked = true;
        }
        self.queue(id);
    }

    /// End a stream both ways, now. What was waiting to be sent is dropped.
    pub fn reset(&mut self, id: u32) {
        if let Some(s) = self.streams.remove(&id) {
            self.control.push_back(frame(id, s.kind, RESET, &[]));
        }
    }

    /// Whoever held the stream has let go of it. A stream both ends have
    /// finished is left to send what it still holds; any other is ended.
    pub fn release(&mut self, id: u32) {
        let finished = self
            .streams
            .get(&id)
            .is_some_and(|s| s.fin_asked && s.peer_fin);
        if !finished {
            self.reset(id);
        }
    }

    /// `n` bytes that arrived on a stream have been used (written on, or
    /// read): the other end is allowed that many more, told in batches.
    pub fn consumed(&mut self, id: u32, n: usize) {
        let Some(s) = self.streams.get_mut(&id) else {
            return;
        };
        s.used = s.used.saturating_add(n as u32);
        if s.used >= WINDOW / 4 && !s.peer_fin {
            let credit = s.used.min(WINDOW - s.recv_window);
            s.used = 0;
            s.recv_window += credit;
            self.control
                .push_back(frame(id, s.kind, CREDIT, &credit.to_be_bytes()));
        }
    }

    /// Bytes to write to the other end: about `max` of them, or fewer.
    /// Empty: nothing is waiting. Frames of the link itself go first; then
    /// one frame from each stream that has any, in turn.
    pub fn output(&mut self, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < max {
            if let Some(f) = self.control.pop_front() {
                out.extend(f);
                continue;
            }
            let Some(id) = self.round.pop_front() else {
                break;
            };
            let Some(s) = self.streams.get_mut(&id) else {
                continue;
            };
            s.in_round = false;
            let n = s.out.len().min(SEND_FRAME);
            let payload: Vec<u8> = s.out.drain(..n).collect();
            let mut flags = 0;
            if !s.open_sent {
                s.open_sent = true;
                flags |= OPEN;
            }
            if s.fin_asked && !s.fin_sent && s.out.is_empty() {
                s.fin_sent = true;
                flags |= FIN;
            }
            if flags != 0 || !payload.is_empty() {
                out.extend(frame(id, s.kind, flags, &payload));
            }
            if s.done() {
                self.streams.remove(&id);
            } else {
                self.queue(id);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Carry everything waiting from one end to the other.
    fn carry(from: &mut Mux, to: &mut Mux) -> Result<usize, MuxError> {
        let mut total = 0;
        loop {
            let bytes = from.output(64 * 1024);
            if bytes.is_empty() {
                return Ok(total);
            }
            total += bytes.len();
            to.feed(&bytes)?;
        }
    }

    fn events(m: &mut Mux) -> Vec<Event> {
        std::iter::from_fn(|| m.event()).collect()
    }

    fn pair() -> (Mux, Mux) {
        let (mut near, mut far) = (Mux::near(), Mux::far());
        near.hello(json!({"station": "laptop"}));
        far.hello(json!({"station": "devbox"}));
        carry(&mut near, &mut far).unwrap();
        carry(&mut far, &mut near).unwrap();
        assert!(matches!(events(&mut near)[..], [Event::Hello(_)]));
        assert!(matches!(events(&mut far)[..], [Event::Hello(_)]));
        (near, far)
    }

    fn data(evs: &[Event], stream: u32) -> Vec<u8> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Data { id, bytes } if *id == stream => Some(bytes.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The owner's rule for a link: "a pane printing megabytes must not
    /// delay a keystroke". A stream nobody reads is sent one window and no
    /// more; a second stream's bytes arrive meanwhile, and when the first
    /// is read again all of it arrives, in order.
    #[test]
    fn a_stream_nobody_reads_holds_one_window_and_never_another_stream() {
        let (mut near, mut far) = pair();
        let flood = near.open(kind::WEB).unwrap();
        let keys = near.open(kind::WEB).unwrap();
        let megabytes: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        // The window is all that is taken.
        let took = near.send(flood, &megabytes);
        assert_eq!(took, WINDOW as usize);
        assert_eq!(near.send(flood, &megabytes[took..]), 0);
        // A key on the other stream is in the first bytes written: one
        // frame of the flood goes before it, not a window of them.
        assert_eq!(near.send(keys, b"k"), 1);
        let first = near.output(1);
        far.feed(&first).unwrap();
        let second = near.output(1);
        assert!(second.len() <= HEADER + 1, "{}", second.len());
        far.feed(&second).unwrap();
        assert_eq!(data(&events(&mut far), keys), b"k");
        // The far end reads nothing of the flood: no more is sent, however
        // long that lasts, and the other stream still works both ways.
        carry(&mut near, &mut far).unwrap();
        let arrived = data(&events(&mut far), flood);
        assert_eq!(arrived.len() + first.len() - HEADER, WINDOW as usize);
        assert_eq!(near.send(flood, &megabytes[took..]), 0);
        assert_eq!(far.send(keys, b"echo"), 4);
        carry(&mut far, &mut near).unwrap();
        assert_eq!(data(&events(&mut near), keys), b"echo");
        // It is read at last: the rest follows, and nothing is lost or
        // out of order.
        let mut got = megabytes[..took].to_vec();
        let mut sent = took;
        far.consumed(flood, took);
        while got.len() < megabytes.len() {
            carry(&mut far, &mut near).unwrap();
            events(&mut near);
            sent += near.send(flood, &megabytes[sent..]);
            carry(&mut near, &mut far).unwrap();
            let more = data(&events(&mut far), flood);
            assert!(!more.is_empty(), "stalled at {}", got.len());
            far.consumed(flood, more.len());
            got.extend(more);
        }
        assert!(got == megabytes);
    }

    /// The window is the other end's promise too: a far end that sends
    /// more than it was allowed is cut off, not buffered without limit.
    #[test]
    fn more_than_the_window_ends_the_link() {
        let (mut near, mut far) = pair();
        let id = near.open(kind::WEB).unwrap();
        carry(&mut near, &mut far).unwrap();
        let chunk = vec![0u8; SEND_FRAME];
        let mut sent = 0;
        let ended = loop {
            // Frames written by hand: an honest end would not send them.
            match near.feed(&frame(id, kind::WEB, 0, &chunk)) {
                Ok(()) => sent += chunk.len(),
                Err(e) => break e,
            }
            assert!(sent <= WINDOW as usize, "taken past the window");
        };
        assert_eq!(ended, MuxError::WindowOverrun(id));
    }

    /// The carrier is another program (ssh) and can end at any byte. An
    /// ending between frames is an ending; one inside a frame is said as
    /// what it is, and what came whole before it was delivered.
    #[test]
    fn a_cut_in_the_middle_of_a_frame_is_told_apart_from_an_ending() {
        let (mut near, mut far) = pair();
        let id = near.open(kind::WEB).unwrap();
        near.send(id, b"whole");
        carry(&mut near, &mut far).unwrap();
        assert_eq!(far.eof(), Ok(()));
        near.send(id, b"never arrives whole");
        let bytes = near.output(1024);
        for cut in 1..bytes.len() {
            let mut far = Mux::far();
            let mut hello = Mux::near();
            hello.hello(json!({}));
            far.feed(&hello.output(1024)).unwrap();
            far.feed(&frame(id, kind::WEB, OPEN, b"whole")).unwrap();
            far.feed(&bytes[..cut]).unwrap();
            assert_eq!(far.eof(), Err(MuxError::CutMidFrame), "cut at {cut}");
            assert_eq!(data(&events(&mut far), id), b"whole");
        }
        // And a carrier that ended before it said anything at all.
        assert_eq!(Mux::near().eof(), Err(MuxError::NotALink(String::new())));
    }

    /// Deck mode's rule (DECISIONS.md, "A station reached over the owner's
    /// own SSH"): the station the owner is at refuses every stream the far
    /// end tries to open, whatever its kind, and the link goes on.
    #[test]
    fn the_near_end_refuses_every_stream_the_far_end_opens() {
        let (mut near, mut far) = pair();
        let mine = near.open(kind::WEB).unwrap();
        carry(&mut near, &mut far).unwrap();
        events(&mut far);
        for kind in [kind::WEB, kind::PING, kind::SIGN, 4, 5, 6, 200] {
            let id = far.open(kind).unwrap();
            far.send(id, b"let me in");
            carry(&mut far, &mut near).unwrap();
            assert_eq!(events(&mut near), vec![Event::Refused { kind }]);
            carry(&mut near, &mut far).unwrap();
            assert_eq!(events(&mut far), vec![Event::Reset { id }]);
            assert!(!far.has(id));
        }
        assert_eq!(near.refused(), 7);
        assert_eq!(near.streams(), 1);
        // The stream the near end opened is untouched.
        far.send(mine, b"still here");
        carry(&mut far, &mut near).unwrap();
        assert_eq!(data(&events(&mut near), mine), b"still here");
    }

    /// The parser faces the other end: a header that names a frame longer
    /// than any may be is refused by the header alone, before a byte of it
    /// is kept.
    #[test]
    fn a_frame_longer_than_any_may_be_is_refused_by_its_header() {
        let (mut near, _) = pair();
        let mut header = frame(2, kind::WEB, 0, &[]);
        header[6..10].copy_from_slice(&((MAX_FRAME + 1) as u32).to_be_bytes());
        assert_eq!(
            near.feed(&header),
            Err(MuxError::FrameTooLarge(MAX_FRAME + 1))
        );
        // The longest allowed is taken (on a stream of this end's own).
        let (mut near, _) = pair();
        let id = near.open(kind::WEB).unwrap();
        assert_eq!(
            near.feed(&frame(id, kind::WEB, 0, &vec![0; MAX_FRAME])),
            Ok(())
        );
    }

    /// "Mismatched versions fail with a plain sentence", and so does a far
    /// side that is no spandeck at all: a shell saying it cannot find the
    /// command, a login banner.
    #[test]
    fn another_version_and_what_is_no_link_at_all_are_said_plainly() {
        let mut near = Mux::near();
        let mut theirs = MAGIC.to_vec();
        theirs.extend(frame(0, kind::LINK, 0, br#"{"proto":2}"#));
        let e = near.feed(&theirs).unwrap_err();
        assert_eq!(e, MuxError::Version { theirs: 2, ours: 1 });
        assert_eq!(
            e.to_string(),
            "the other end speaks version 2 of the link and this one speaks version 1: update the older spandeck"
        );
        let mut near = Mux::near();
        let e = near
            .feed(b"sh: 1: spandeck: not found\n\x1b[31mmore")
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "the other end did not begin a link; it said: sh: 1: spandeck: not found"
        );
    }

    /// A link carries a bounded number of streams: the far end takes no
    /// more than that, and refuses the next without ending the others.
    #[test]
    fn streams_past_the_limit_are_refused_one_by_one() {
        let (mut near, mut far) = pair();
        // Written by hand: the near end itself opens no more than the limit.
        for n in 0..MAX_STREAMS as u32 + 3 {
            far.feed(&frame(1 + 2 * n, kind::WEB, OPEN, &[])).unwrap();
        }
        assert_eq!(far.streams(), MAX_STREAMS);
        assert_eq!(far.refused(), 3);
        assert!((0..MAX_STREAMS).all(|_| near.open(kind::WEB).is_some()));
        assert!(near.open(kind::WEB).is_none());
    }

    /// Both ends finishing a stream lets go of it, so a link that has
    /// carried a thousand connections holds none of them.
    #[test]
    fn a_stream_both_ends_finished_is_let_go() {
        let (mut near, mut far) = pair();
        for _ in 0..1000 {
            let id = near.open(kind::WEB).unwrap();
            near.send(id, b"GET / HTTP/1.1\r\n\r\n");
            near.fin(id);
            carry(&mut near, &mut far).unwrap();
            far.send(id, b"HTTP/1.1 200 OK\r\n\r\n");
            far.fin(id);
            carry(&mut far, &mut near).unwrap();
            events(&mut near);
            events(&mut far);
        }
        assert_eq!((near.streams(), far.streams()), (0, 0));
    }
}
