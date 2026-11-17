//! HTTPS on the station's own Tailscale name (DECISIONS.md, "HTTPS on every
//! station, the app saved on the device, and carrying on through another
//! station").
//!
//! This module has the certificate: asking Tailscale for it, keeping a
//! copy, knowing when it runs out, and saying how things stand. The web
//! crate serves TLS with what is held here. It also has the one command
//! that is ever run with an administrator's right: giving the station's
//! own user the right to ask Tailscale (`tailnet.operator`).
//!
//! The key is never logged, is in no answer, and never leaves the station.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use spandeck_terminal::sys;

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::paths::now_ms;

/// Said in `caps` while the TLS port is answered.
pub const CAP: &str = "https";
/// The switch for a page carrying on through this station, and what a
/// station that allows it says in `caps`.
pub const VIA_FLAG: &str = "page_failover";
pub const VIA_CAP: &str = "deck_via";
const DAY_MS: u64 = 24 * 3_600_000;
/// Asked again this often, whatever is held: Tailscale renews in the last
/// third of a certificate's ninety days.
pub const ASK_EVERY: Duration = Duration::from_secs(12 * 3600);
/// With less than this left, Tailscale is told how long the answer must
/// still be good for.
const RENEW_BELOW_MS: u64 = 21 * DAY_MS;
/// From here the days left are a warning.
pub const WARN_BELOW_DAYS: i64 = 14;
/// How long `tailscale cert` is given: a first certificate is issued by
/// Let's Encrypt while it waits.
const FETCH_LIMIT: Duration = Duration::from_secs(120);
const SUDO_LIMIT: Duration = Duration::from_secs(20);
/// `tailnet.operator`: tries a minute.
const TRIES_A_MINUTE: usize = 3;

/// A certificate chain and its key, as PEM text.
pub struct Pem {
    pub cert: String,
    key: String,
    /// The name it is served under.
    pub name: String,
    pub expires_ms: Option<u64>,
}

impl Pem {
    /// For the listener only.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for Pem {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

/// Overwrite a text that held a secret.
fn wipe(text: &mut String) {
    // Zeros are valid UTF-8, so the text stays a text.
    unsafe { text.as_bytes_mut() }.fill(0);
    text.clear();
}

/// A password held for one command. Wiped when dropped. Deliberately has
/// no `Debug`, `Display`, `Clone` or `Serialize` (the rule of Add a
/// machine's SSH password, `add::Secret`, kept apart from it only because
/// that one's bytes are private to its module).
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(mut text: String) -> Secret {
        let s = Secret(text.as_bytes().to_vec());
        wipe(&mut text);
        s
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    /// on | off | needs_operator | not_enabled | no_tailscale | starting | error
    state: &'static str,
    name: Option<String>,
    expires_ms: Option<u64>,
    why: String,
}

pub struct HttpsState {
    status: Mutex<Status>,
    /// What is served: the web crate watches this.
    pub pem: tokio::sync::watch::Sender<Option<Arc<Pem>>>,
    /// The TLS port is being answered (said by the web crate).
    serving: std::sync::atomic::AtomicBool,
    /// Ask now: the owner has just given the right to.
    pub kick: tokio::sync::Notify,
    asked: Mutex<Option<Instant>>,
    tries: Mutex<Vec<u64>>,
}

impl Default for HttpsState {
    fn default() -> Self {
        HttpsState {
            status: Mutex::new(Status {
                state: "starting",
                name: None,
                expires_ms: None,
                why: String::new(),
            }),
            pem: tokio::sync::watch::Sender::new(None),
            serving: Default::default(),
            kick: Default::default(),
            asked: Mutex::new(None),
            tries: Mutex::new(vec![]),
        }
    }
}

impl HttpsState {
    pub fn set_serving(&self, on: bool) {
        self.serving.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn serving(&self) -> bool {
        self.serving.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Why no certificate was had.
#[derive(Debug, PartialEq, Eq)]
enum NoCert {
    /// Tailscale gives certificates only to root or its operator user.
    NeedsOperator,
    /// HTTPS certificates are not switched on for the tailnet.
    NotEnabled,
    NoTailscale,
    Other(String),
}

/// Text another program printed, made safe to show: one line, bounded, no
/// control characters.
fn said(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

/// What `tailscale cert` meant by failing.
fn why_no_cert(stderr: &str) -> NoCert {
    let low = stderr.to_lowercase();
    if low.contains("access denied") || low.contains("permission denied") {
        NoCert::NeedsOperator
    } else if low.contains("does not support")
        || low.contains("https cert")
        || low.contains("not enabled")
        || low.contains("invalid domain")
    {
        NoCert::NotEnabled
    } else if [
        "failed to connect",
        "not running",
        "is stopped",
        "appear to be running",
    ]
    .iter()
    .any(|w| low.contains(w))
    {
        NoCert::NoTailscale
    } else {
        NoCert::Other(said(stderr))
    }
}

/// The certificate's own blocks and the key's, out of what was printed, in
/// whichever order. What comes back has one line end, `\n`, whatever the
/// program printed (`tailscale.exe` may well print `\r\n`), and ends with
/// one: what is kept and served is the same on every system.
fn split_pem(text: &str) -> Option<(String, String)> {
    let (mut cert, mut key) = (String::new(), String::new());
    let mut block = String::new();
    let mut kind: Option<bool> = None;
    // A byte-order mark, as a Windows program may put first, is no part of it.
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if let Some(label) = line.strip_prefix("-----BEGIN ") {
            kind = if label.starts_with("CERTIFICATE") {
                Some(true)
            } else if label.contains("PRIVATE KEY") {
                Some(false)
            } else {
                None
            };
            block.clear();
        }
        if kind.is_some() {
            block.push_str(line);
            block.push('\n');
        }
        if line.starts_with("-----END ") {
            match kind.take() {
                Some(true) => cert.push_str(&block),
                Some(false) if key.is_empty() => key.push_str(&block),
                _ => {}
            }
            wipe(&mut block);
        }
    }
    (!cert.is_empty() && !key.is_empty()).then_some((cert, key))
}

/// When the first certificate of a PEM chain stops being valid.
pub fn not_after_ms(cert_pem: &str) -> Option<u64> {
    let body: String = cert_pem
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END"))
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .ok()?;
    // Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version,
    // serialNumber, signature, issuer, validity SEQUENCE { notBefore,
    // notAfter }, … }, … }
    let (_, cert, _) = tlv(&der)?;
    let (_, mut tbs, _) = tlv(cert)?;
    let (tag, _, rest) = tlv(tbs)?;
    if tag == 0xa0 {
        tbs = rest;
    }
    for _ in 0..3 {
        tbs = tlv(tbs)?.2;
    }
    let (_, validity, _) = tlv(tbs)?;
    let (_, _, after) = tlv(validity)?;
    let (tag, when, _) = tlv(after)?;
    let text = std::str::from_utf8(when).ok()?;
    let digits = text.strip_suffix('Z')?;
    let n = |at: usize, len: usize| digits.get(at..at + len)?.parse::<i64>().ok();
    // UTCTime has two digits of year, GeneralizedTime four.
    let (year, at) = match tag {
        0x17 => {
            let y = n(0, 2)?;
            (if y >= 50 { 1900 + y } else { 2000 + y }, 2)
        }
        0x18 => (n(0, 4)?, 4),
        _ => return None,
    };
    let rfc = format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        n(at, 2)?,
        n(at + 2, 2)?,
        n(at + 4, 2)?,
        n(at + 6, 2)?,
        n(at + 8, 2)?
    );
    crate::tailnet::parse_rfc3339(&rfc)
}

/// One DER element: its tag, its contents, and what follows it.
fn tlv(der: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = der.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |a, b| (a << 8) | *b as usize);
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Run a program to its end, or for `limit`: what it printed, and how it
/// ended (`None`: it was ended here). `input` is written to it and its
/// input closed.
fn run_bounded(
    cmd: &mut Command,
    input: Option<&[u8]>,
    limit: Duration,
) -> std::io::Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    sys::no_window(cmd);
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(mut to), Some(input)) = (child.stdin.take(), input) {
        // A program that does not read gets nothing; it is ended below.
        let _ = to.write_all(input);
    }
    let read = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut out = vec![];
            let _ = from.by_ref().take(1 << 20).read_to_end(&mut out);
            out
        })
    };
    let out = read(Box::new(child.stdout.take().expect("piped")));
    let err = read(Box::new(child.stderr.take().expect("piped")));
    let began = Instant::now();
    let code = loop {
        match child.try_wait()? {
            Some(status) => break status.code().or(Some(-1)),
            None if began.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    Ok((
        code,
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    ))
}

/// The name a certificate can be had for: the one of the tailnet's
/// certificate names that is this machine's own.
fn cert_name(status: &Value) -> std::result::Result<String, NoCert> {
    let own = status
        .pointer("/Self/DNSName")
        .and_then(|d| d.as_str())
        .map(|d| d.trim_end_matches('.').to_lowercase())
        .filter(|d| !d.is_empty())
        .ok_or(NoCert::NoTailscale)?;
    status
        .get("CertDomains")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str())
        .map(|c| c.trim_end_matches('.').to_lowercase())
        .find(|c| *c == own)
        .ok_or(NoCert::NotEnabled)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// The name and port of a station's HTTPS deck, as that station said them:
/// kept only if they have the shape of a name and a port. What is done
/// with the name is decided against this station's own Tailscale client
/// (`member_decks`), never by this.
pub fn brief(info: &Value) -> Value {
    let https = &info["https"];
    let name = https["name"].as_str().map(str::to_lowercase);
    let port = https["port"].as_u64().filter(|p| (1..=65535).contains(p));
    // A station that says it does not serve it has no HTTPS deck.
    let on = https["on"].as_bool().unwrap_or(true);
    match (name, port) {
        (Some(name), Some(port)) if on && valid_name(&name) => json!({"name": name, "port": port}),
        _ => Value::Null,
    }
}

/// The origins of a member's deck: over HTTP at the address this station
/// itself looks at; over HTTPS at the name this station's own Tailscale
/// client gives for that address (for a station on this machine:
/// `localhost`), with the port the member announced.
pub fn deck_origins(addr: &str, https: &Value, tailscale_name: Option<&str>) -> Vec<String> {
    let mut out = vec![format!("http://{}", addr.to_lowercase())];
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    let local = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if let (Some(name), Some(port)) = (https["name"].as_str(), https["port"].as_u64()) {
        if tailscale_name == Some(name) || (local && name == "localhost") {
            out.push(format!("https://{name}:{port}"));
        }
    }
    out
}

// ------------------------------------------------- the administrator's right

/// How `sudo` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Sudo {
    Done,
    WrongPassword,
    /// This user may not use sudo, or not for this.
    NotAllowed,
    NeedsTerminal,
    /// Something was asked that is not the password: its words.
    OtherPrompt(String),
    TimedOut,
    Failed(String),
}

/// The one command: exactly this vector, whatever is asked.
pub fn operator_command(sudo: &Path, tailscale: &Path, user: &str) -> Vec<std::ffi::OsString> {
    vec![
        sudo.into(),
        "-S".into(),
        "-k".into(),
        "-p".into(),
        "".into(),
        tailscale.into(),
        "set".into(),
        format!("--operator={user}").into(),
    ]
}

/// What sudo's end and words mean. With `-p ''` the password is asked for
/// with no words at all, so words that are not sudo's own are a question
/// of another kind (a second factor), which standard input cannot answer.
pub fn sudo_outcome(code: Option<i32>, stderr: &str) -> Sudo {
    let Some(code) = code else {
        return Sudo::TimedOut;
    };
    if code == 0 {
        return Sudo::Done;
    }
    let low = stderr.to_lowercase();
    if low.contains("a terminal is required") || low.contains("must have a tty") {
        return Sudo::NeedsTerminal;
    }
    if low.contains("not in the sudoers")
        || low.contains("not allowed to")
        || low.contains("may not run sudo")
    {
        return Sudo::NotAllowed;
    }
    let other = stderr
        .lines()
        .map(str::trim)
        .find(|l| {
            let l = l.to_lowercase();
            !l.is_empty() && !l.starts_with("sudo:") && !l.starts_with("sorry, try again")
        })
        .map(said);
    let wrong = low.contains("incorrect password")
        || low.contains("sorry, try again")
        || low.contains("no password was provided");
    match (other, wrong) {
        (Some(words), true) => Sudo::OtherPrompt(words),
        (_, true) => Sudo::WrongPassword,
        _ => Sudo::Failed(said(stderr)),
    }
}

/// Run it: the password goes to sudo's standard input and nowhere else.
pub fn run_operator(argv: &[std::ffi::OsString], password: &Secret) -> Sudo {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .current_dir("/");
    let mut input = password.0.clone();
    input.push(b'\n');
    let ran = run_bounded(&mut cmd, Some(&input), SUDO_LIMIT);
    input.fill(0);
    match ran {
        Ok((code, _, err)) => sudo_outcome(code, &String::from_utf8_lossy(&err)),
        Err(e) => Sudo::Failed(e.to_string()),
    }
}

/// A program of the system's by one of these names: the first that is
/// there, and only if it is root's own (`sys::system_program`).
fn system_program(places: &[&str]) -> std::result::Result<PathBuf, bool> {
    // Debug builds only, for the tests: a folder whose `sudo` and
    // `tailscale` stand in for the system's.
    if cfg!(debug_assertions) {
        if let Some(dir) = std::env::var_os("SPANDECK_TEST_SYSTEM_DIR") {
            let name = Path::new(places[0]).file_name().expect("a file name");
            let at = Path::new(&dir).join(name);
            return if at.is_file() { Ok(at) } else { Err(false) };
        }
    }
    let mut found = false;
    for place in places {
        let Ok(real) = std::fs::canonicalize(place) else {
            continue;
        };
        found = true;
        if sys::system_program(&real) {
            return Ok(real);
        }
    }
    // `true`: it is there, and is not the system's.
    Err(found)
}

const SUDO_PLACES: &[&str] = &["/usr/bin/sudo", "/bin/sudo", "/usr/local/bin/sudo"];
const TAILSCALE_PLACES: &[&str] = &[
    "/usr/bin/tailscale",
    "/usr/sbin/tailscale",
    "/bin/tailscale",
    "/usr/local/bin/tailscale",
];

impl Daemon {
    /// Is HTTPS wanted here at all? Off by the owner's switch; and a
    /// station that does not use Tailscale (`discovery = false`) has no
    /// certificate to ask for unless its config names a command.
    fn https_wanted(&self) -> bool {
        self.cfg.web.https != Some(false) && (self.cfg.discovery || self.cfg.web.cert_cmd.is_some())
    }

    fn tls_dir(&self) -> PathBuf {
        self.paths.state_dir.join("tls")
    }

    fn https_status(&self, state: &'static str, why: impl Into<String>) {
        let held = self.https.pem.borrow().clone();
        let mut s = self.https.status.lock().unwrap();
        s.state = state;
        s.why = why.into();
        if let Some(p) = held {
            s.name = Some(p.name.clone());
            s.expires_ms = p.expires_ms;
        } else {
            s.expires_ms = None;
        }
    }

    /// Every origin that is the deck of a member of this station's fleet,
    /// with the member's name: what a page may be loaded from and still
    /// connect here (the web crate's `origin_ok`). From this station's own
    /// roster and its own Tailscale client; nothing a request brings.
    pub fn member_decks(&self) -> Vec<(String, String)> {
        let names = self.fleet.names.lock().unwrap().clone();
        let mut members: std::collections::BTreeMap<String, (String, Value)> = self
            .known_stations()
            .into_iter()
            .map(|k| (k.station, (k.addr, k.info["https"].clone())))
            .collect();
        // A member that answers says where its HTTPS deck is now.
        for p in self.fleet.peers() {
            if p.state == "online" || (p.state != "not_linked" && !members.contains_key(&p.station))
            {
                let https = if p.state == "online" {
                    brief(&p.info)
                } else {
                    Value::Null
                };
                members.insert(p.station, (p.addr, https));
            }
        }
        members
            .into_iter()
            .filter(|(station, _)| *station != self.station)
            .flat_map(|(station, (addr, https))| {
                let host = addr.rsplit_once(':').map_or(addr.as_str(), |(h, _)| h);
                deck_origins(&addr, &https, names.get(host).map(String::as_str))
                    .into_iter()
                    .map(move |o| (o, station.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// What `station.info` says of HTTPS here: whether, and where. An
    /// agent may read that answer, so it says no more than the machine's
    /// own `tailscale status` does: not when the certificate runs out.
    pub fn https_brief(&self) -> Value {
        let mut v = self.https_view();
        if let Some(o) = v.as_object_mut() {
            o.remove("expires_ms");
            o.remove("days_left");
        }
        v
    }

    /// All of it, for the owner and the fleet (`https.status`, `perf.get`,
    /// `spandeck doctor`). Nothing of the key.
    pub fn https_view(&self) -> Value {
        let s = self.https.status.lock().unwrap().clone();
        let on = self.https.serving();
        let now = now_ms();
        json!({
            "on": on,
            "name": s.name,
            "port": self.cfg.web.https_port,
            "expires_ms": s.expires_ms,
            "days_left": s.expires_ms.map(|e| crate::tailnet::days_left(e, now)),
            "state": if on && s.state != "on" { "on" } else { s.state },
            "why": s.why,
            // The one tap: only where it is the way (Linux and the like).
            "user": (s.state == "needs_operator").then(sys::user_name).flatten(),
        })
    }

    /// The copy kept from before, if it is whole.
    fn https_load(&self) -> Option<Pem> {
        let dir = self.tls_dir();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        let (cert, key, name) = (read("cert.pem")?, read("key.pem")?, read("name")?);
        let name = name.trim().to_string();
        valid_name(&name).then(|| Pem {
            expires_ms: not_after_ms(&cert),
            cert,
            key,
            name,
        })
    }

    fn https_save(&self, pem: &Pem) {
        let dir = self.tls_dir();
        let wrote = std::fs::create_dir_all(&dir)
            .and_then(|_| sys::private_dir(&dir))
            .and_then(|_| sys::write_private(&dir.join("key.pem"), pem.key.as_bytes()))
            .and_then(|_| sys::write_private(&dir.join("cert.pem"), pem.cert.as_bytes()))
            .and_then(|_| sys::write_private(&dir.join("name"), pem.name.as_bytes()));
        if let Err(e) = wrote {
            tracing::warn!(error = %e, "could not keep a copy of the HTTPS certificate");
        }
    }

    /// Ask for the certificate. Blocking: `tailscale cert` may wait for it
    /// to be issued.
    fn https_fetch(&self, name: &str, min_validity: bool) -> std::result::Result<Pem, NoCert> {
        let mut cmd = match &self.cfg.web.cert_cmd {
            Some(argv) if !argv.is_empty() => {
                let mut cmd = Command::new(&argv[0]);
                cmd.args(argv[1..].iter().map(|a| a.replace("{name}", name)));
                cmd
            }
            // A debug build asks Tailscale for nothing: a certificate that
            // is issued is published, and no test may cause one.
            _ if cfg!(debug_assertions) => {
                return Err(NoCert::Other(
                    "a debug build asks Tailscale for no certificate".into(),
                ))
            }
            _ => {
                static BIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
                let bin = BIN
                    .get_or_init(crate::fleet::tailscale_bin)
                    .as_ref()
                    .ok_or(NoCert::NoTailscale)?;
                let mut cmd = crate::fleet::tailscale_cmd(bin);
                cmd.args(["cert", "--cert-file", "-", "--key-file", "-"]);
                if min_validity {
                    cmd.args(["--min-validity", "504h"]);
                }
                cmd.arg(name);
                cmd
            }
        };
        let (code, out, err) = run_bounded(&mut cmd, None, FETCH_LIMIT)
            .map_err(|e| NoCert::Other(said(&e.to_string())))?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        let split = split_pem(&text);
        wipe(&mut text);
        match (code, split) {
            (Some(0), Some((cert, key))) => Ok(Pem {
                expires_ms: not_after_ms(&cert),
                cert,
                key,
                name: name.to_string(),
            }),
            (Some(0), None) => Err(NoCert::Other("no certificate and key were printed".into())),
            (None, _) => Err(NoCert::Other(
                "Tailscale did not answer within 2 minutes".into(),
            )),
            _ => Err(why_no_cert(&String::from_utf8_lossy(&err))),
        }
    }

    /// Bring what is held up to date: at start, every twelve hours, when
    /// the machine's name changes, and when the owner has just given the
    /// right to ask. Blocking. `force`: ask whatever was asked lately.
    pub fn https_refresh(&self, force: bool) {
        if !self.https_wanted() {
            let off = self.cfg.web.https == Some(false);
            if off {
                // The owner's switch: the key is not kept either.
                let _ = std::fs::remove_dir_all(self.tls_dir());
            }
            self.https.pem.send_replace(None);
            self.https_status(
                "off",
                if off {
                    "turned off in this station's config ([web] https = false)"
                } else {
                    "this station does not use Tailscale (discovery = false)"
                },
            );
            return;
        }
        if self.https.pem.borrow().is_none() {
            if let Some(kept) = self.https_load() {
                self.https.pem.send_replace(Some(Arc::new(kept)));
                self.https_status("on", "");
            }
        }
        let name = match &self.cfg.web.name {
            Some(name) if valid_name(name) => Ok(name.to_lowercase()),
            Some(_) => Err(NoCert::Other("[web] name is not a host name".into())),
            None => crate::fleet::tailscale_status()
                .ok_or(NoCert::NoTailscale)
                .and_then(|s| cert_name(&s)),
        };
        let held = self.https.pem.borrow().clone();
        let now = now_ms();
        let left = held
            .as_ref()
            .and_then(|p| p.expires_ms)
            .map_or(0, |e| e.saturating_sub(now));
        let got = name.and_then(|name| {
            let same = held.as_ref().is_some_and(|p| p.name == name);
            let lately = self
                .https
                .asked
                .lock()
                .unwrap()
                .is_some_and(|at| at.elapsed() < ASK_EVERY);
            if same && lately && !force && left >= RENEW_BELOW_MS {
                return Ok(None);
            }
            *self.https.asked.lock().unwrap() = Some(Instant::now());
            self.https_fetch(&name, same && left < RENEW_BELOW_MS)
                .or_else(|e| match e {
                    // A Tailscale from before `--min-validity`.
                    NoCert::Other(_) if same && left < RENEW_BELOW_MS => {
                        self.https_fetch(&name, false)
                    }
                    e => Err(e),
                })
                .map(Some)
        });
        match got {
            Ok(None) => {}
            Ok(Some(pem)) => {
                let changed = held.as_ref().is_none_or(|h| h.cert != pem.cert);
                if changed {
                    self.https_save(&pem);
                    self.record(
                        "https.certificate",
                        None,
                        None,
                        None,
                        None,
                        json!({"name": pem.name, "expires_ms": pem.expires_ms, "renewed": held.is_some()}),
                    );
                    self.https.pem.send_replace(Some(Arc::new(pem)));
                }
                self.https_status("on", "");
            }
            // What is held is still served; why a new one was not had is said.
            Err(e) => {
                let (state, why) = match e {
                    NoCert::NeedsOperator => ("needs_operator", "Tailscale gives this machine's certificate only to an administrator or to the user it was told to trust".to_string()),
                    NoCert::NotEnabled => ("not_enabled", "HTTPS certificates are not switched on for your Tailscale network (its admin console: DNS, HTTPS Certificates)".to_string()),
                    NoCert::NoTailscale => ("no_tailscale", "Tailscale is not answering on this machine".to_string()),
                    NoCert::Other(why) => ("error", why),
                };
                if held.is_some() {
                    self.https_status("on", format!("not renewed: {why}"));
                } else {
                    self.https_status(state, why);
                }
            }
        }
    }

    /// `tailnet.operator`: let this station's own user ask Tailscale for
    /// the machine's certificate (or, `off`, take that back). The owner's
    /// alone, at the station it is for; the password is used for the one
    /// command and dropped. Blocking.
    pub fn tailnet_operator(&self, password: Option<Secret>, off: bool, by: &str) -> Result<Value> {
        let log = |outcome: &str| {
            self.record(
                "tailnet.operator",
                None,
                None,
                None,
                Some(by),
                json!({"off": off, "outcome": outcome}),
            );
        };
        if sys::WINDOWS {
            return Err(invalid(
                "not needed on Windows: Tailscale gives the certificate to the user who is signed in",
            ));
        }
        // The owner at this station's own deck or command line, or a
        // signed-in browser connected to this station: never a request
        // that another station passed on or made in the owner's name.
        if by.starts_with("you (via ") {
            return Err(Error::Denied(
                "a password is taken only by the station it is for: connect to that station's own Control Deck".into(),
            ));
        }
        let Some(password) = password else {
            return Err(invalid("missing parameter: password"));
        };
        // Three wrong answers a minute, then a wait.
        let tried = |add: bool| {
            let now = now_ms();
            let mut tries = self.https.tries.lock().unwrap();
            tries.retain(|at| now.saturating_sub(*at) < 60_000);
            if add {
                tries.push(now);
            }
            tries.len()
        };
        if tried(false) >= TRIES_A_MINUTE {
            log("too_many");
            return Err(Error::Replayed {
                code: "too_many",
                message: "Three wrong tries in a minute: wait a minute, then try again.".into(),
            });
        }
        let station = &self.station;
        let user = sys::user_name()
            .filter(|u| {
                u.len() <= 64
                    && !u.starts_with('-')
                    && u.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            })
            .ok_or_else(|| invalid("this station could not tell which user it runs as"))?;
        let by_hand = format!(
            "At {station} itself, run: sudo tailscale set --operator={}",
            if off { "" } else { &user }
        );
        // (`Replayed` is the error that carries a code of its own.)
        let problem = |code: &'static str, message: String| {
            log(code);
            Err(Error::Replayed { code, message })
        };
        let sudo = match system_program(SUDO_PLACES) {
            Ok(p) => p,
            Err(_) => {
                return problem(
                    "no_sudo",
                    format!("{station} has no sudo where a system program should be, so this cannot be done from the deck. {by_hand} (as an administrator)."),
                )
            }
        };
        let tailscale = match system_program(TAILSCALE_PLACES) {
            Ok(p) => p,
            Err(_) => {
                return problem(
                    "no_tailscale",
                    format!("Tailscale's program on {station} is not where a system program should be, or is not the system's own, so spandeck will not run it as an administrator. {by_hand}"),
                )
            }
        };
        let argv = operator_command(&sudo, &tailscale, if off { "" } else { &user });
        let outcome = run_operator(&argv, &password);
        drop(password);
        if matches!(outcome, Sudo::WrongPassword | Sudo::OtherPrompt(_)) {
            tried(true);
        }
        match outcome {
            Sudo::Done => {
                log("done");
                // Ask for the certificate now (or stop being able to).
                self.https.kick.notify_one();
                Ok(json!({"done": true, "off": off, "user": user}))
            }
            Sudo::WrongPassword => problem(
                "wrong_password",
                format!("That is not {user}'s password on {station}. Nothing was changed; try again."),
            ),
            Sudo::NotAllowed => problem(
                "not_allowed",
                format!("{user} may not use sudo on {station}, so this needs an administrator there. {by_hand} (as an administrator)."),
            ),
            Sudo::NeedsTerminal => problem(
                "needs_terminal",
                format!("sudo on {station} only works from a terminal (its `requiretty` setting), so it cannot be asked from the deck. {by_hand}"),
            ),
            Sudo::OtherPrompt(words) => problem(
                "other_prompt",
                format!("sudo on {station} asked for something that is not a password ({words:?}), which the deck cannot answer. {by_hand}"),
            ),
            Sudo::TimedOut => problem(
                "timed_out",
                format!("sudo on {station} did not finish within 20 seconds and was ended. Nothing is known to have changed. {by_hand}"),
            ),
            Sudo::Failed(words) => problem(
                "failed",
                format!("The command did not work on {station}: {words:?}. {by_hand}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A certificate made for these tests (`CN=localhost`); its key was
    // thrown away.
    // (It is only ever read for its dates here; nothing verifies it.)
    const CERT_FILE: &str = include_str!("../tests/fixtures/https/cert.pem");

    /// The fixture with `\n` line ends, however it was checked out.
    fn cert() -> String {
        CERT_FILE.replace("\r\n", "\n")
    }

    /// Reason: "The days left are on the Performance page and in `spandeck
    /// doctor`" and renewal is decided by them: read from the certificate
    /// itself, wherever it came from.
    #[test]
    fn a_certificates_end_is_read_from_it() {
        let ms = not_after_ms(&cert()).expect("its end");
        // The same from a file with Windows line ends.
        assert_eq!(not_after_ms(&cert().replace('\n', "\r\n")), Some(ms));
        assert_eq!(
            crate::tailnet::rfc3339(ms),
            include_str!("../tests/fixtures/https/not_after.txt").trim()
        );
        assert_eq!(
            not_after_ms("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"),
            None
        );
        assert_eq!(not_after_ms("nothing"), None);
    }

    /// Reason: the certificate and the key are taken from one program's
    /// output, in whichever order it prints them; anything else is not a
    /// certificate.
    #[test]
    fn what_tailscale_prints_is_split_into_the_chain_and_the_key() {
        let cert = cert();
        let key = "-----BEGIN EC PRIVATE KEY-----\nMHc=\n-----END EC PRIVATE KEY-----\n";
        let chain = format!("{cert}{cert}");
        let both = [format!("{cert}{cert}{key}"), format!("{key}{cert}{cert}")];
        for text in &both {
            // As printed; with Windows line ends (found on the Windows
            // station, 17 Nov 2026: the fixture was checked out so, and
            // `tailscale.exe` may print so); with no line end after the
            // last line; with a byte-order mark and blank lines around.
            // What is kept is the same text every time.
            let crlf = text.replace('\n', "\r\n");
            let padded = format!("\u{feff}\r\n  {}\r\n\r\n", crlf.trim_end());
            for form in [
                text.as_str(),
                &crlf,
                text.trim_end(),
                crlf.trim_end(),
                &padded,
            ] {
                let (got_chain, got_key) = split_pem(form).expect("both");
                assert_eq!(got_chain, chain, "{form:?}");
                assert_eq!(got_key, key, "{form:?}");
                assert!(!got_chain.contains('\r') && !got_key.contains('\r'));
            }
        }
        assert!(split_pem(&cert).is_none());
        assert!(split_pem(key).is_none());
    }

    /// Reason: "Who may ask, per platform": what the deck says, and whether
    /// it offers the one tap, follows from why Tailscale refused.
    #[test]
    fn why_tailscale_gave_no_certificate() {
        assert_eq!(
            why_no_cert("Access denied: cert access denied\n\nUse 'sudo tailscale cert' or 'tailscale up --operator=$USER' to not require root.\n"),
            NoCert::NeedsOperator
        );
        assert_eq!(
            why_no_cert("500 Internal Server Error: your Tailscale account does not support getting TLS certs"),
            NoCert::NotEnabled
        );
        assert_eq!(
            why_no_cert("failed to connect to local tailscaled; it doesn't appear to be running"),
            NoCert::NoTailscale
        );
        assert_eq!(
            why_no_cert("something\u{7} else\nmore"),
            NoCert::Other("something else".into())
        );
        let status = json!({"Self": {"DNSName": "Spark2.tail0.ts.net."}, "CertDomains": ["spark2.tail0.ts.net"]});
        assert_eq!(cert_name(&status).unwrap(), "spark2.tail0.ts.net");
        let off = json!({"Self": {"DNSName": "spark2.tail0.ts.net."}, "CertDomains": null});
        assert_eq!(cert_name(&off), Err(NoCert::NotEnabled));
        assert_eq!(cert_name(&json!({})), Err(NoCert::NoTailscale));
    }

    /// Reason: "the Origin is the deck of a station on this station's own
    /// roster of members ... where the name is the one this station's own
    /// Tailscale client gives for that address and the port is the one
    /// that member announced ... Never a name or address a request brings."
    #[test]
    fn a_members_deck_is_named_by_this_stations_own_tailscale() {
        let said = json!({"https": {"on": true, "name": "M4.tail0.ts.net", "port": 7392, "expires_ms": 1}});
        let https = brief(&said);
        assert_eq!(https, json!({"name": "m4.tail0.ts.net", "port": 7392}));
        assert_eq!(
            deck_origins("100.64.0.2:7391", &https, Some("m4.tail0.ts.net")),
            ["http://100.64.0.2:7391", "https://m4.tail0.ts.net:7392"]
        );
        // A name the member says and Tailscale does not give for its
        // address is no deck's; nor is one with no reading at all.
        assert_eq!(
            deck_origins("100.64.0.2:7391", &https, Some("other.tail0.ts.net")),
            ["http://100.64.0.2:7391"]
        );
        assert_eq!(
            deck_origins("100.64.0.2:7391", &https, None),
            ["http://100.64.0.2:7391"]
        );
        // Stations on one machine (the tests): `localhost`, and only there.
        let local = brief(&json!({"https": {"name": "localhost", "port": 9001}}));
        assert_eq!(
            deck_origins("127.0.0.1:9000", &local, None),
            ["http://127.0.0.1:9000", "https://localhost:9001"]
        );
        assert_eq!(
            deck_origins("100.64.0.2:7391", &local, None),
            ["http://100.64.0.2:7391"]
        );
        // What is not a name and a port is not kept; nor is HTTPS that is off.
        for bad in [
            json!({"https": {"name": "a b", "port": 7392}}),
            json!({"https": {"name": "evil.example/x", "port": 7392}}),
            json!({"https": {"name": "m4.tail0.ts.net", "port": 70000}}),
            json!({"https": {"on": false, "name": "m4.tail0.ts.net", "port": 7392}}),
            json!({}),
        ] {
            assert_eq!(brief(&bad), Value::Null, "{bad}");
        }
    }

    /// Reason: "Exactly one command ... An argument vector, no shell", and
    /// "What is told apart": the words sudo ends with decide what the owner
    /// is told.
    #[test]
    fn the_one_command_and_what_sudo_said() {
        let argv = operator_command(
            Path::new("/usr/bin/sudo"),
            Path::new("/usr/bin/tailscale"),
            "mb",
        );
        assert_eq!(
            argv,
            [
                "/usr/bin/sudo",
                "-S",
                "-k",
                "-p",
                "",
                "/usr/bin/tailscale",
                "set",
                "--operator=mb"
            ]
        );
        assert_eq!(sudo_outcome(Some(0), ""), Sudo::Done);
        assert_eq!(
            sudo_outcome(Some(1), "Sorry, try again.\nsudo: no password was provided\nsudo: 1 incorrect password attempt\n"),
            Sudo::WrongPassword
        );
        assert_eq!(
            sudo_outcome(Some(1), "mb is not in the sudoers file.\nThis incident has been reported to the administrator.\n"),
            Sudo::NotAllowed
        );
        assert_eq!(
            sudo_outcome(Some(1), "Sorry, user mb is not allowed to execute '/usr/bin/tailscale set --operator=mb' as root on spark2.\n"),
            Sudo::NotAllowed
        );
        assert_eq!(
            sudo_outcome(Some(1), "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper\n"),
            Sudo::NeedsTerminal
        );
        assert_eq!(
            sudo_outcome(
                Some(1),
                "Verification code: \nsudo: 1 incorrect password attempt\n"
            ),
            Sudo::OtherPrompt("Verification code:".into())
        );
        assert_eq!(sudo_outcome(None, ""), Sudo::TimedOut);
        assert_eq!(
            sudo_outcome(Some(1), "sudo: unable to resolve host\n"),
            Sudo::Failed("sudo: unable to resolve host".into())
        );
    }

    /// Reason: "the password goes to sudo's standard input and nowhere
    /// else": a stand-in for sudo records its arguments, its environment
    /// and what it read.
    #[cfg(unix)]
    #[test]
    fn the_password_is_only_on_standard_input() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sudo = dir.path().join("sudo");
        let seen = dir.path().join("seen");
        std::fs::write(
            &sudo,
            format!(
                "#!/bin/sh\n{{ echo \"args:$*\"; /usr/bin/env; echo \"stdin:$(/bin/cat)\"; }} > {}\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = operator_command(&sudo, Path::new("/usr/bin/tailscale"), "mb");
        let out = run_operator(&argv, &Secret::new("hunter2 pw".into()));
        assert_eq!(out, Sudo::Done);
        let seen = std::fs::read_to_string(seen).unwrap();
        assert!(
            seen.contains("args:-S -k -p  /usr/bin/tailscale set --operator=mb\n"),
            "{seen}"
        );
        assert!(seen.contains("stdin:hunter2 pw\n"), "{seen}");
        assert_eq!(seen.matches("hunter2").count(), 1, "{seen}");
        assert!(
            seen.contains("LC_ALL=C\n") && !seen.contains("HOME="),
            "{seen}"
        );
    }
}
