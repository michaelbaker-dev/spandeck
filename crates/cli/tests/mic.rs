//! The microphone of the message box (DECISIONS.md, "The microphone").
//! The owner's report, 17 Nov 2026, from an iPhone: "I tapped it on a
//! spark2 session and it locked up spandeck I had to close the web page
//! and restart."
//!
//! No iPhone can be driven from here, so what is checked is that nothing
//! the browser's dictation does, or fails to do, can leave the page
//! unusable: with a stand-in for `SpeechRecognition` that throws, that
//! never answers, that reports results without end, and that never says
//! it stopped. And the owner's own speech program: a recording is turned
//! into text by a stand-in command, for the owner alone, and is deleted.
//!
//! Merge tier: a station and a browser, about fifteen seconds.
//!
//!   cargo test -p spandeck --test mic -- --ignored
#![cfg(unix)]
mod common;
use common::browser::Page;
use common::*;
use serde_json::{json, Value};

/// A stand-in for the browser's dictation, and for its recorder. `mode`
/// says how the next one behaves.
const FAKES: &str = r#"(() => {
  window.__mic = { made: [], mode: 'ok', tracks: 0 };
  class Fake {
    constructor() { __mic.made.push(this); }
    start() {
      const m = __mic.mode;
      if (m === 'throw') { const e = new Error('no'); e.name = 'NotAllowedError'; throw e; }
      if (m === 'silent') return;
      setTimeout(() => { this.onstart && this.onstart({}); if (m === 'storm') this.storm(); }, 10);
    }
    stop() { this.stopped = true; clearInterval(this.iv); if (__mic.mode !== 'deaf') setTimeout(() => this.onend && this.onend({}), 10); }
    abort() { this.aborted = true; clearInterval(this.iv); }
    storm() { let n = 0; this.iv = setInterval(() => { for (let i = 0; i < 300; i++) { n++; this.onresult && this.onresult({ results: [[{ transcript: 'word ' + n }]] }); } __mic.stormed = n; }, 0); }
    say(t) { this.onresult && this.onresult({ results: [[{ transcript: t }]] }); }
  }
  window.SpeechRecognition = window.webkitSpeechRecognition = Fake;
  __mic.perm = 'granted';
  navigator.permissions.query = async () => ({ state: __mic.perm });
  __mic.btn = () => [...document.querySelectorAll('.chat-mic')].find(b => !b.closest('[hidden]'));
  __mic.box = () => __mic.btn().closest('form').querySelector('.chat-box');
  __mic.said = () => { const p = __mic.btn().closest('form').querySelector('.mic-said'); return p.hidden ? '' : p.innerText; };
  __mic.state = () => __mic.btn().dataset.mic || null;
  __mic.last = () => __mic.made[__mic.made.length - 1];
  __mic.usable = () => { const b = __mic.box(), s = __mic.btn().closest('form').querySelector('.chat-send'); return !b.disabled && !b.readOnly && !s.disabled && !__mic.btn().disabled; };
  return true;
})()"#;

const RECORDER: &str = r#"(() => {
  navigator.mediaDevices.getUserMedia = async () => ({ getTracks: () => [{ stop() { __mic.tracks += 1; } }] });
  window.MediaRecorder = class {
    static isTypeSupported(m) { return m === 'audio/mp4'; }
    constructor() { this.mimeType = 'audio/mp4'; this.state = 'inactive'; }
    start() { this.state = 'recording'; }
    stop() { this.state = 'inactive'; setTimeout(() => { this.ondataavailable && this.ondataavailable({ data: new Blob(['a stand-in for sound'], { type: 'audio/mp4' }) }); this.onstop && this.onstop(); }, 5); }
  };
  return true;
})()"#;

fn idle(page: &Page, what: &str) {
    page.wait(
        what,
        "__mic.state() === null && __mic.btn().getAttribute('aria-pressed') === 'false' && __mic.usable()",
    );
}

fn reports(s: &Station, ended: &str) -> Vec<Value> {
    s.log("client.report")
        .into_iter()
        .map(|r| r["data"]["report"].clone())
        .filter(|r| r["what"] == "mic" && r["ended"] == ended)
        .collect()
}

#[test]
#[ignore = "merge tier: a station and a browser (about ten seconds)"]
fn nothing_the_browsers_dictation_does_leaves_the_message_box_unusable() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": []}), json!({"name": "talker"}));
    s.wait_text(&pane, "[fake] session");
    let page = Page::open(&s, 390, 844);
    page.phone_screen();
    page.goto(&format!("#/s/alpha/p/{pane}"));
    page.wait(
        "the message box",
        "[...document.querySelectorAll('.chat-box')].some(b => !b.closest('[hidden]'))",
    );
    page.js(FAKES);

    // A start that throws: said in words, and the box is as it was.
    page.js("(__mic.mode = 'throw', __mic.btn().click(), true)");
    idle(&page, "after a start that threw");
    assert!(
        page.js("__mic.said()")
            .as_str()
            .unwrap()
            .starts_with("Dictation was not allowed"),
        "{}",
        page.js("__mic.said()")
    );

    // A start that is answered by nothing: Stop is on the button at once,
    // and after a second and a half it is given up by itself.
    page.js("(__mic.mode = 'silent', __mic.btn().click(), true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.btn().getAttribute('aria-label'), __mic.usable()]"),
        json!(["starting", "Stop dictating", true])
    );
    idle(&page, "a start that was never answered is given up");
    assert!(
        page.js("__mic.said()")
            .as_str()
            .unwrap()
            .contains("did not start"),
        "{}",
        page.js("__mic.said()")
    );
    assert_eq!(page.js("__mic.last().aborted"), json!(true));
    // While the browser may be asking for permission it is waited for,
    // with Stop on the button; a tap ends the wait.
    page.js("(__mic.perm = 'prompt', __mic.btn().click(), true)");
    page.wait(
        "waiting for the browser's question",
        "__mic.state() === 'asking' && __mic.said().includes('answer its question')",
    );
    page.js("(__mic.btn().click(), __mic.perm = 'granted', true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.usable()]"),
        json!([null, true])
    );

    // Results without end, three hundred at a time: the page still
    // answers, the box is written, and a tap stops it at once.
    page.js("(__mic.mode = 'storm', __mic.btn().click(), true)");
    page.wait(
        "what is heard, in the box",
        "__mic.state() === 'listening' && __mic.stormed > 3000 && /^word \\d+$/.test(__mic.box().value) && __mic.usable()",
    );
    page.js("(__mic.btn().click(), true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.btn().getAttribute('aria-pressed'), __mic.last().stopped]"),
        json!([null, "false", true]),
        "a tap stops a storm at once"
    );
    idle(&page, "after the storm");
    // (The last words are taken for a moment after Stop.)
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after = page.js("__mic.box().value");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        page.js("__mic.box().value"),
        after,
        "nothing is written after Stop"
    );
    // And what the owner types meanwhile is theirs: dictation stops, and
    // nothing is written over it.
    page.js("(__mic.box().value = '', __mic.box().dispatchEvent(new Event('input')), __mic.btn().click(), true)");
    page.wait(
        "the storm again",
        "__mic.state() === 'listening' && /^word/.test(__mic.box().value)",
    );
    page.js("(b => (b.value = 'typed by hand', b.dispatchEvent(new Event('input')), true))(__mic.box())");
    page.wait(
        "dictation stops when the owner types",
        "__mic.state() === null && __mic.said().includes('when you typed')",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(page.js("__mic.box().value"), json!("typed by hand"));
    page.js("(__mic.box().value = '', __mic.box().dispatchEvent(new Event('input')), true)");

    // A browser that never says it stopped: the button is free at once,
    // and the browser is told to give up a moment later.
    page.js("(__mic.mode = 'deaf', __mic.btn().click(), true)");
    page.wait("listening", "__mic.state() === 'listening'");
    page.js("(__mic.last().say('the last words'), __mic.btn().click(), true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.usable()]"),
        json!([null, true])
    );
    page.wait("given up on", "__mic.last().aborted === true");
    assert_eq!(page.js("__mic.box().value"), json!("the last words"));
    page.js("(__mic.box().value = '', __mic.box().dispatchEvent(new Event('input')), __mic.mode = 'ok', true)");

    // Held, and the finger's lifting never heard of: it listens, the next
    // tap stops it, and left alone it ends by itself.
    let down = "__mic.btn().dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }))";
    page.js(&format!("({down}, true)"));
    page.wait("held: listening", "__mic.state() === 'listening'");
    page.js("(__mic.btn().dispatchEvent(new PointerEvent('pointerdown', { bubbles: true })), __mic.btn().click(), true)");
    idle(&page, "the next tap stops a hold that was never let go");
    page.js(&format!(
        "(window.spandeckMicTimes = {{ cap: 500 }}, {down}, true)"
    ));
    page.wait("held again", "__mic.state() === 'listening'");
    page.wait(
        "it ends by itself",
        "__mic.state() === null && __mic.said().includes('stops by itself')",
    );
    page.js("(delete window.spandeckMicTimes, true)");
    // Lifted somewhere else on the page.
    page.js(&format!("({down}, true)"));
    page.wait("held a third time", "__mic.state() === 'listening'");
    page.js(
        "(document.body.dispatchEvent(new PointerEvent('pointerup', { bubbles: true })), true)",
    );
    idle(&page, "a finger lifted elsewhere ends the hold");

    // The page is hidden while it listens.
    // (The click that follows a hold's release is not a tap: wait it out.)
    std::thread::sleep(std::time::Duration::from_millis(600));
    page.js("(__mic.btn().click(), true)");
    page.wait("listening", "__mic.state() === 'listening'");
    page.js("(Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true }), document.dispatchEvent(new Event('visibilitychange')), true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.last().aborted, __mic.usable()]"),
        json!([null, true, true])
    );
    page.js("(delete document.visibilityState, document.dispatchEvent(new Event('visibilitychange')), true)");

    // Not a secure page: nothing is tried, and it is said why. No
    // dictation in the browser: the same, in other words.
    let made = page.js("__mic.made.length");
    page.js("(Object.defineProperty(window, 'isSecureContext', { value: false, configurable: true }), __mic.btn().click(), true)");
    assert_eq!(
        page.js("[window.isSecureContext, __mic.state(), __mic.made.length]"),
        json!([false, null, made])
    );
    assert!(
        page.js("__mic.said()")
            .as_str()
            .unwrap()
            .starts_with("The microphone is not available on this address"),
        "{}",
        page.js("__mic.said()")
    );
    page.js("(Object.defineProperty(window, 'isSecureContext', { value: true, configurable: true }), delete window.SpeechRecognition, delete window.webkitSpeechRecognition, window.SpeechRecognition = window.webkitSpeechRecognition = undefined, __mic.btn().click(), true)");
    assert!(
        page.js("__mic.said()")
            .as_str()
            .unwrap()
            .starts_with("This browser has no dictation"),
        "{}",
        page.js("__mic.said()")
    );
    assert_eq!(
        page.js("[__mic.state(), __mic.made.length, __mic.usable()]"),
        json!([null, made, true])
    );

    // Nothing was sent to the pane by any of it.
    assert!(!s.text(&pane).contains("word "), "dictation sends nothing");

    // Each attempt was told to the station, with what happened and when.
    let threw = wait_for("the reports", 10, || {
        let r = reports(&s, "threw");
        (!r.is_empty() && !reports(&s, "absent").is_empty()).then_some(r)
    });
    assert_eq!(threw[0]["error"], "NotAllowedError", "{threw:?}");
    assert_eq!(threw[0]["engine"], "speech");
    assert_eq!(threw[0]["secure"], true);
    assert_eq!(threw[0]["standalone"], false);
    assert!(
        threw[0]["ua"].as_str().unwrap().starts_with("chrome "),
        "{threw:?}"
    );
    let dog = reports(&s, "watchdog");
    assert_eq!(dog.len(), 1, "{dog:?}");
    assert_eq!(dog[0]["events"][0][0], "starting");
    assert!(
        dog[0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[0] == "start()"),
        "{dog:?}"
    );
    assert!(dog[0]["ms"].as_u64().unwrap() >= 1400, "{dog:?}");
    let tap = reports(&s, "tap");
    assert!(
        tap.iter().any(|r| r["results"].as_u64().unwrap_or(0) > 3000
            && r["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e[0] == "onstart")),
        "{tap:?}"
    );
    for (ended, how) in [
        ("typed", "tap"),
        ("cap", "hold"),
        ("lifted", "hold"),
        ("hidden", "tap"),
        ("insecure", "tap"),
    ] {
        let r = reports(&s, ended);
        assert!(r.iter().any(|r| r["how"] == how), "{ended}: {r:?}");
    }
}

/// The owner's speech program: a stand-in that prints fixed text, and
/// notes the file it was given.
fn speech_station(dir: &std::path::Path, body: &str) -> Station {
    let script = dir.join("speech.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$1\" >> {}\ntest -s \"$1\" || exit 9\n{body}\n",
            dir.join("seen").display()
        ),
    )
    .unwrap();
    Station::start_with(
        "alpha",
        &format!(
            "\n[speech]\ncommand = [\"/bin/sh\", {}]\nmax_bytes = 4096\n",
            json!(script)
        ),
        &[],
    )
}

fn clip(bytes: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(vec![7u8; bytes])
}

#[test]
#[ignore = "merge tier: two stations (about three seconds)"]
fn a_recording_is_turned_into_text_for_the_owner_alone_and_deleted() {
    // Off until it is configured: not offered, and refused.
    let plain = Station::start("beta");
    let caps = |s: &Station| s.call("station.info", json!({}))["caps"].clone();
    assert!(!caps(&plain).as_array().unwrap().contains(&json!("speech")));
    let no = plain
        .try_call(
            "speech.transcribe",
            json!({"data_b64": clip(10), "mime": "audio/mp4"}),
        )
        .unwrap_err()
        .to_string();
    assert!(no.contains("has no speech command"), "{no}");
    drop(plain);

    let dir = tempfile::tempdir().unwrap();
    let s = speech_station(dir.path(), "case \"$1\" in *.webm) echo 'no model for this' >&2; exit 3;; esac\necho '  the voles are counted  '");
    assert!(caps(&s).as_array().unwrap().contains(&json!("speech")));
    let got = s.call(
        "speech.transcribe",
        json!({"data_b64": clip(2000), "mime": "audio/mp4", "seconds": 4.2}),
    );
    assert_eq!(got["text"], "the voles are counted");
    let speech_dir = s.root.join("state/speech");
    let left = || {
        std::fs::read_dir(&speech_dir)
            .map(|d| d.count())
            .unwrap_or(0)
    };
    let seen = || std::fs::read_to_string(dir.path().join("seen")).unwrap_or_default();
    let given = seen().trim().to_string();
    assert!(
        given.starts_with(speech_dir.to_str().unwrap()) && given.ends_with(".m4a"),
        "{given}"
    );
    assert_eq!(left(), 0, "the recording is deleted once it is text");

    // A program that fails: said, and the recording is deleted all the same.
    let failed = s
        .try_call(
            "speech.transcribe",
            json!({"data_b64": clip(100), "mime": "audio/webm;codecs=opus"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        failed.contains("the speech command failed") && failed.contains("no model for this"),
        "{failed}"
    );
    assert_eq!(left(), 0);

    // Too large, too long, or not a recording: refused before anything is
    // written or run.
    let runs = seen().lines().count();
    for (params, why) in [
        (
            json!({"data_b64": clip(5000), "mime": "audio/mp4"}),
            "larger than 4 KB",
        ),
        (
            json!({"data_b64": clip(10), "mime": "audio/mp4", "seconds": 90}),
            "longer than 60 s",
        ),
        (
            json!({"data_b64": clip(10), "mime": "application/x-sh"}),
            "not taken",
        ),
        (json!({"data_b64": "", "mime": "audio/mp4"}), "empty"),
    ] {
        let e = s
            .try_call("speech.transcribe", params)
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{why}: {e}");
    }
    assert_eq!(seen().lines().count(), runs);
    assert_eq!(left(), 0);

    // An agent in a pane may not hand the station a clip.
    let pane = s.fake(json!({"steps": []}), json!({"name": "agent"}));
    let agent = s
        .call_as_pane(
            &pane,
            "speech.transcribe",
            json!({"data_b64": clip(10), "mime": "audio/mp4"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        agent.contains("not allowed") || agent.contains("denied"),
        "{agent}"
    );
    assert_eq!(seen().lines().count(), runs);

    // The log says that it happened, and nothing of what was said.
    let log = s.log("speech.transcribed");
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(!json!(log).to_string().contains("voles"));
}

#[test]
#[ignore = "merge tier: a station and a browser (about five seconds)"]
fn a_recording_from_the_deck_fills_the_box_and_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = speech_station(dir.path(), "echo 'the voles are counted'");
    let pane = s.fake(json!({"steps": []}), json!({"name": "talker"}));
    s.wait_text(&pane, "[fake] session");
    let page = Page::open(&s, 390, 844);
    page.phone_screen();
    page.goto(&format!("#/s/alpha/p/{pane}"));
    page.wait(
        "the message box",
        "[...document.querySelectorAll('.chat-box')].some(b => !b.closest('[hidden]'))",
    );
    page.js(FAKES);
    page.js(RECORDER);
    page.js("(__mic.box().value = 'first,', __mic.box().dispatchEvent(new Event('input')), true)");
    // The station says it can, so the page records for it. (This browser
    // has no dictation of its own here: until the page has heard what the
    // station can do, a tap only says so.)
    page.js("(window.SpeechRecognition = window.webkitSpeechRecognition = undefined, true)");
    page.wait(
        "recording",
        "__mic.state() ? __mic.state() === 'recording' && __mic.said().startsWith('Recording, for alpha') && __mic.usable() : (__mic.btn().click(), false)",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    page.js("(__mic.btn().click(), true)");
    page.wait(
        "the text, in the box",
        "__mic.state() === null && __mic.box().value === 'first, the voles are counted'",
    );
    assert_eq!(
        page.js("[__mic.made.length, __mic.tracks > 0]"),
        json!([0, true]),
        "the browser's dictation was not asked, and the microphone was let go"
    );
    assert_eq!(
        std::fs::read_dir(s.root.join("state/speech"))
            .unwrap()
            .count(),
        0
    );
    // Not sent: it waits in the box for the owner.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !s.text(&pane).contains("voles"),
        "what was said is never sent by itself"
    );

    // Cancelled while the station works: nothing is added.
    page.js("(__mic.btn().click(), true)");
    page.wait("recording again", "__mic.state() === 'recording'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    page.js("(__mic.btn().click(), __mic.btn().click(), true)");
    assert_eq!(
        page.js("[__mic.state(), __mic.said()]"),
        json!([null, "Cancelled: nothing was added."])
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        page.js("__mic.box().value"),
        json!("first, the voles are counted")
    );
    let done = wait_for("the report", 10, || reports(&s, "done").into_iter().next());
    assert_eq!(done["engine"], "record");
    assert_eq!(done["station"], "alpha");
    assert_eq!(done["chars"], 21);
}
