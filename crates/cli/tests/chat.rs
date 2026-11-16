//! Chat for every pane (DECISIONS.md, "Chat for every pane";
//! `docs/conversation-api.md`). The owner's report of 16 Nov 2026, from a
//! phone: "Every window should have the chat and terminal option, looks
//! like it's just on a Claude window. Even in layouts". A Codex pane showed
//! only "Terminal only: Codex has not said which session it is in yet
//! (nothing has come from its hooks)".
//!
//! The stand-in agent keeps its conversation as Codex 0.169.0 does (a
//! session file begun with the first message, in the shapes of
//! `crates/archive/fixtures/codex-0.169.0.jsonl`) and runs no hooks, as a
//! Codex whose hooks the owner has not reviewed. Nothing here runs the real
//! program.
//!
//! The two station scenarios run with every change. The two in a browser
//! are the merge tier:
//!
//!   cargo test -p spandeck --test chat -- --ignored
mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

const CODEX_FIXTURE: &str = include_str!("../../archive/fixtures/codex-0.169.0.jsonl");

fn folder(s: &Station, name: &str) -> PathBuf {
    let dir = s.root.join("home").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A pane of `agent` that runs the stand-in program.
fn pane(s: &Station, agent: &str, name: &str, dir: &Path, script: Value) -> String {
    let path = s.script_file(&script);
    s.call(
        "pane.spawn",
        json!({"agent": agent, "name": name, "program": FAKE, "args": ["--script", path], "cwd": dir}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A Codex whose hooks never run: it answers each message in its session
/// file and on its screen.
fn hookless_codex() -> Value {
    json!({"codex": true, "no_hooks": true, "steps": [],
        "on_input": [{"codex_reply": "counted what you asked"}]})
}

fn send(s: &Station, pane: &str, text: &str) {
    s.call(
        "pane.paste",
        json!({"pane": pane, "text": text, "enter": true}),
    );
    s.wait_text(pane, &format!("[fake] got: {text}"));
}

/// What a deck from before this asks, and what one of today asks.
fn conversation(s: &Station, pane: &str, screen: bool) -> Value {
    let mut params = json!({"pane": pane});
    if screen {
        params["screen"] = json!(true);
    }
    s.call("pane.conversation", params)
}

/// `[kind, text]` of each item.
fn said(answer: &Value) -> Vec<(String, String)> {
    answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap_or("").to_string(),
                i["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn transcript_of(s: &Station, pane: &str) -> Value {
    wait_for_or(
        "the pane's conversation from its session file",
        15,
        || {
            let mut files = vec![];
            let mut dirs = vec![s.root.join("home/.codex")];
            while let Some(d) = dirs.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    if e.path().is_dir() {
                        dirs.push(e.path());
                    } else {
                        files.push(e.path().display().to_string());
                    }
                }
            }
            format!("{}\nfiles: {files:?}", conversation(s, pane, true))
        },
        || {
            let a = conversation(s, pane, true);
            (a["source"] == json!("transcript")
                && a["items"]
                    .as_array()
                    .is_some_and(|i| i.iter().any(|x| x["kind"] == json!("text"))))
            .then_some(a)
        },
    )
}

/// The report: a Codex pane had no Chat because its hooks had not run. Its
/// session is found among Codex's own files, strictly: begun since the
/// pane's program started, in the pane's folder, and no other pane's. Two
/// Codex panes in one folder are never given each other's conversation, a
/// session a hook names takes the place of one that was found, and until
/// there is a session the screen is the conversation.
#[test]
fn a_codex_pane_with_no_hooks_gets_its_conversation_from_its_session_file() {
    let s = Station::start("alpha");
    let weir = folder(&s, "weir");
    let first = pane(&s, "codex", "first", &weir, hookless_codex());
    s.wait_text(&first, "[fake] session");

    // Before anything is said Codex has no session and no file. A deck
    // from before is told why there is no conversation, as it always was;
    // a deck of today is given the screen, with the same reason.
    let old = conversation(&s, &first, false);
    assert_eq!(old["available"], json!(false), "{old}");
    let why = old["reason"].as_str().unwrap();
    assert!(
        why.starts_with("no conversation for this pane: Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's for this folder has appeared since the pane started (Codex begins one with its first message)"),
        "{why}"
    );
    let new = conversation(&s, &first, true);
    assert_eq!(
        (&new["available"], &new["source"], new["reason"].as_str()),
        (&json!(true), &json!("screen"), Some(why)),
        "{new}"
    );
    assert!(new["session"].as_str().unwrap().starts_with("screen:"));

    // The first message begins its session file, and the conversation is
    // read from it: what was said, not what the screen shows.
    send(&s, &first, "How many otters?");
    let a = transcript_of(&s, &first);
    assert_eq!(
        said(&a),
        vec![
            ("user".to_string(), "How many otters?".to_string()),
            ("text".to_string(), "counted what you asked".to_string())
        ],
        "{a}"
    );
    assert_eq!(a["session_from"], json!("files"));
    assert_eq!(a["agent"], json!("codex"));
    // A deck that held the screen's conversation is told to start again.
    let moved = s.call(
        "pane.conversation",
        json!({"pane": first, "screen": true, "session": new["session"], "after": new["cursor"]}),
    );
    assert_eq!(moved["reset"], json!(true), "{moved}");
    // A deck from before gets the same conversation: nothing new is asked of it.
    assert_eq!(said(&conversation(&s, &first, false)), said(&a));
    // The pane list says it has something not read, as for any conversation.
    assert_eq!(s.pane(&first)["unread"], json!(true));

    // A second Codex in the same folder. Its session file is begun after
    // both programs started, so it could be either pane's: it is given to
    // neither. The second pane says why and shows its screen; the first
    // keeps its own conversation, and never shows the other's message.
    let second = pane(&s, "codex", "second", &weir, hookless_codex());
    s.wait_text(&second, "[fake] session");
    send(&s, &second, "And the voles?");
    // (Long enough for the files to be looked at again, were it to find one.)
    std::thread::sleep(std::time::Duration::from_millis(700));
    let b = conversation(&s, &second, true);
    assert_eq!(b["source"], json!("screen"), "{b}");
    assert!(
        b["reason"].as_str().unwrap().contains(
            "another pane runs Codex in this folder and reports nothing either, so a session file begun since both started could be either pane's: spandeck does not guess"
        ),
        "{b}"
    );
    let screen = said(&b);
    assert!(
        screen.contains(&("user".to_string(), "And the voles?".to_string())),
        "{b}"
    );
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let a = conversation(&s, &first, true);
    assert_eq!(a["source"], json!("transcript"), "{a}");
    let all = a.to_string();
    assert!(
        all.contains("How many otters?") && !all.contains("voles"),
        "{a}"
    );

    // A Codex in a folder of its own, beside those two: found. Then one of
    // its hooks runs and names another session (one saved before the pane
    // started, which the files would never have given it): the hook wins.
    let marsh = folder(&s, "marsh");
    let saved = s.root.join("saved-rollout.jsonl");
    std::fs::write(&saved, CODEX_FIXTURE).unwrap();
    let third = pane(
        &s,
        "codex",
        "third",
        &marsh,
        json!({"codex": true, "no_hooks": true, "steps": [
            {"prompt": "Count the herons"}, {"codex_reply": "nine herons"},
            {"wait_file": "go"},
            {"name_session": {"id": "01a1da7a-0000-7000-8000-00000000c0de", "path": saved}}]}),
    );
    let c = transcript_of(&s, &third);
    assert_eq!(said(&c)[0].1, "Count the herons", "{c}");
    assert_eq!(c["session_from"], json!("files"));
    std::fs::write(marsh.join("go"), "").unwrap();
    let named = wait_for("the session its hook named", 15, || {
        let a = s.call(
            "pane.conversation",
            json!({"pane": third, "screen": true, "session": c["session"], "after": c["cursor"]}),
        );
        (a["session"] == json!("01a1da7a-0000-7000-8000-00000000c0de")).then_some(a)
    });
    assert_eq!(
        (&named["reset"], &named["session_from"], &named["source"]),
        (&json!(true), &json!("hooks"), &json!("transcript")),
        "{named}"
    );
    assert_eq!(said(&named)[0].1, "How many otters are in the weir log?");
    assert!(!named.to_string().contains("herons"), "{named}");

    // An agent is refused its own pane's conversation, the screen's too.
    let err = s
        .call_as_pane(
            &second,
            "pane.conversation",
            json!({"pane": second, "screen": true}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

/// The report: "Every window should have the chat and terminal option." A
/// pane with no transcript (here a program that is no agent) is given as
/// what was sent to it and what it printed after each, in order; a block
/// keeps its id while it grows and after; a caller that asks from its
/// cursor is told only what is new.
#[test]
fn a_shell_pane_is_served_as_what_was_sent_and_what_it_printed() {
    let s = Station::start("alpha");
    let dir = folder(&s, "plain");
    let sh = pane(
        &s,
        "shell",
        "plain",
        &dir,
        json!({"no_hooks": true, "steps": [], "on_input": [{"say": "done with it"}]}),
    );
    s.wait_text(&sh, "[fake] session");
    // A deck from before is answered as before.
    let old = conversation(&s, &sh, false);
    assert_eq!(
        (&old["available"], old["reason"].as_str()),
        (
            &json!(false),
            Some("no conversation for this pane: it runs a shell, not an agent")
        )
    );
    send(&s, &sh, "list the reeds");
    s.wait_text(&sh, "done with it");
    let a = conversation(&s, &sh, true);
    assert_eq!(
        (&a["available"], &a["source"], &a["agent"]),
        (&json!(true), &json!("screen"), &json!("shell")),
        "{a}"
    );
    assert_eq!(a["reason"], old["reason"]);
    let items = said(&a);
    let kinds: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    assert_eq!(kinds, vec!["screen", "user", "screen"], "{a}");
    assert!(items[0].1.contains("[fake] session"), "{a}");
    assert_eq!(items[1].1, "list the reeds");
    assert!(
        items[2].1.contains("[fake] got: list the reeds") && items[2].1.contains("done with it"),
        "{a}"
    );
    assert_eq!(a["items"][1]["origin"], json!("you"));
    assert_eq!(a["items"][2]["open"], json!(true));
    let open = a["items"][2]["id"].clone();

    // Asked again from its cursor with nothing printed since: nothing.
    let ask = |a: &Value| {
        s.call(
            "pane.conversation",
            json!({"pane": sh, "screen": true, "session": a["session"], "after": a["cursor"]}),
        )
    };
    let quiet = ask(&a);
    assert_eq!(
        (&quiet["items"], &quiet["updated"], &quiet["reset"]),
        (&json!([]), &json!([]), &json!(false)),
        "{quiet}"
    );
    // A second message: the block that was open comes once more, under the
    // id it had, as it was frozen; the message and what followed are new.
    send(&s, &sh, "count them");
    s.wait_text(&sh, "[fake] session");
    let next = wait_for("the second message's output", 10, || {
        let n = ask(&a);
        (said(&n).len() == 2 && said(&n)[1].1.contains("done with it")).then_some(n)
    });
    assert_eq!(next["updated"].as_array().unwrap().len(), 1, "{next}");
    assert_eq!(next["updated"][0]["id"], open);
    assert_eq!(next["updated"][0].get("open"), None);
    assert!(!next["updated"][0]["text"]
        .as_str()
        .unwrap()
        .contains("count them"));
    assert_eq!(
        said(&next)[0],
        ("user".to_string(), "count them".to_string())
    );
    assert!(
        said(&next)[1].1.contains("[fake] got: count them"),
        "{next}"
    );

    // One block in full.
    let detail = s.call(
        "pane.conversation.detail",
        json!({"pane": sh, "item": open, "session": a["session"]}),
    );
    assert!(
        detail["text"]["text"]
            .as_str()
            .unwrap()
            .contains("[fake] got: list the reeds"),
        "{detail}"
    );
    // An agent in another pane cannot read it.
    let other = pane(&s, "fake", "other", &dir, json!({"steps": []}));
    let err = s
        .call_as_pane(
            &other,
            "pane.conversation",
            json!({"pane": sh, "screen": true}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

/// What the two browser scenarios share: a Codex whose hooks never ran and
/// a real shell, side by side in a layout, and a way to write pictures for
/// a human to look at (`SPANDECK_SHOTS=/some/dir`).
#[cfg(unix)]
mod in_a_browser {
    use super::*;
    use common::browser::Page;

    pub struct Scene {
        pub s: Station,
        pub codex: String,
        pub sh: String,
    }

    pub fn scene() -> Scene {
        let s = Station::start("alpha");
        let weir = folder(&s, "weir");
        let codex = pane(&s, "codex", "counter", &weir, hookless_codex());
        s.wait_text(&codex, "[fake] session");
        let sh = s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": "scratch", "cwd": weir}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.call(
            "deck.layout.save",
            json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
                "a": {"id": codex, "station": "alpha"}, "b": {"id": sh, "station": "alpha"}}}}),
        );
        Scene { s, codex, sh }
    }

    pub fn shoot(page: &Page, name: &str) {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        let Some(dir) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    }

    /// Write `text` in the message box under `root` and press Send.
    pub fn say(root: &str, text: &str) -> String {
        format!(
            "(b => b && !b.closest('[hidden]') && (b.value = {}, b.dispatchEvent(new Event('input')), document.querySelector('{root} .chat-send').click(), true))(document.querySelector('{root} .chat-box'))",
            json!(text)
        )
    }

    /// The owner's report, on the phone it came from: in a layout, a Codex
    /// pane whose hooks have not run and a shell both have "Chat |
    /// Terminal". Chat shows something for each (the screen, with why;
    /// then Codex's own conversation once its session file is there, with
    /// what was being written kept), a message sent from Chat reaches the
    /// pane, and Terminal is one tap away. What a pane printed is put on
    /// the page as text (rule 6).
    #[test]
    #[ignore = "merge tier: a browser"]
    fn on_a_phone_every_pane_of_a_layout_has_chat_and_terminal() {
        let Scene { s, codex, sh } = scene();
        let page = Page::open(&s, 390, 800);
        page.goto("#/l/pair");
        let showing = "document.querySelector('.lay-showing')?.innerText";
        let switch = "[...document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button')].map(b => b.innerText + (b.classList.contains('on') ? '*' : '')).join(' ')";
        let face = |to: &str| {
            format!("[...document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button')].find(b => b.innerText === '{to}')")
        };
        let src = "document.querySelector('.phone-layout .chat-src')?.innerText";
        let blocks = "[...document.querySelectorAll('.phone-layout .chat-screen-text')].map(x => x.innerText).join('\\n')";
        let bubbles = "[...document.querySelectorAll('.phone-layout .chat-user .chat-bubble')].map(x => x.innerText)";

        // The Codex pane, before it has a session: Chat, made from its
        // screen, and saying so where "Terminal only" was said.
        let seen = page.wait(
            "the Codex pane's screen as its conversation",
            &format!("{showing} === 'pair › counter · 1 of 2' && {blocks}.includes('[fake] session') && [{switch}, {src}, !!document.querySelector('.phone-layout .chat-why'), !!document.querySelector('.phone-layout .chat-keys:not([hidden])')]"),
        );
        assert_eq!(seen[0], json!("Chat* Terminal"), "{seen}");
        assert!(
            seen[1].as_str().unwrap().starts_with("From the screen: Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's"),
            "{seen}"
        );
        assert_eq!(
            (&seen[2], &seen[3]),
            (&json!(false), &json!(true)),
            "{seen}"
        );
        shoot(&page, "phone-layout-codex-before-a-session");

        // A message from Chat reaches the pane. Codex begins its session
        // file with it, and the conversation becomes Codex's own: the
        // message as a bubble, the answer as text, no note, and what was
        // being written meanwhile still in the box.
        page.wait(
            "the message sent",
            &say(".phone-layout", "How many otters?"),
        );
        s.wait_text(&codex, "[fake] got: How many otters?");
        page.wait(
            "the box emptied, then something half written",
            "(b => b.value === '' && (b.value = 'and the voles', b.dispatchEvent(new Event('input')), true))(document.querySelector('.phone-layout .chat-box'))",
        );
        let seen = page.wait(
            "Codex's own conversation",
            &format!("[...document.querySelectorAll('.phone-layout .chat-text')].some(t => t.innerText.includes('counted what you asked')) && !document.querySelector('.phone-layout .chat-src') && [{bubbles}, document.querySelector('.phone-layout .chat-box').value, {switch}, document.querySelectorAll('.phone-layout .chat-screen-text').length]"),
        );
        assert_eq!(
            seen,
            json!([["How many otters?"], "and the voles", "Chat* Terminal", 0])
        );
        shoot(&page, "phone-layout-codex-found-session");

        // Terminal is one tap away, and Chat one tap back.
        page.wait(
            "the terminal, then Chat again",
            &format!("({}.click(), true)", face("Terminal")),
        );
        page.wait(
            "the terminal",
            &format!("!!document.querySelector('.phone-layout .xterm-rows') && !document.querySelector('.phone-layout .chat-log') && ({}.click(), true)", face("Chat")),
        );
        page.wait(
            "the conversation again, then the next pane",
            "!!document.querySelector('.phone-layout .chat-log') && !document.querySelector('.phone-layout .xterm-rows') && (document.querySelector('.lay-nav .lay-step:last-child').click(), true)",
        );

        // The shell: it has the switch too. It opens as its terminal (a
        // shell's default), and Chat is one tap.
        let seen = page.wait(
            "the shell with the switch",
            &format!("{showing} === 'pair › scratch · 2 of 2' && !!document.querySelector('.phone-layout .xterm-rows') && {switch} !== '' && [{switch}, !!document.querySelector('.phone-layout .chat-why')]"),
        );
        assert_eq!(seen, json!(["Chat Terminal*", false]));
        page.js(&format!("{}.click()", face("Chat")));
        let seen = page.wait(
            "the shell's screen as its conversation",
            &format!("!!document.querySelector('.phone-layout .chat-log') && !!{src} && [{src}, {switch}]"),
        );
        assert_eq!(
            seen,
            json!([
                "From the screen: it runs a shell, not an agent",
                "Chat* Terminal"
            ])
        );
        page.wait(
            "a command sent from Chat",
            &say(".phone-layout", "echo reed-$((40+2)) '<b id=evil>bold</b>'"),
        );
        s.wait_text(&sh, "reed-42");
        let seen = page.wait(
            "the command and what it printed",
            &format!("{blocks}.includes('reed-42 <b id=evil>bold</b>') && [{bubbles}, !!document.querySelector('#evil'), !!document.querySelector('.phone-layout .chat-keys:not([hidden])')]"),
        );
        assert_eq!(
            seen,
            json!([["echo reed-$((40+2)) '<b id=evil>bold</b>'"], false, true])
        );
        shoot(&page, "phone-layout-shell-chat");
        // A key of the row reaches the pane: Ctrl-C ends a command.
        page.wait(
            "a command that waits",
            &say(".phone-layout", "sleep 300; echo slept"),
        );
        s.wait_text(&sh, "sleep 300");
        page.js("[...document.querySelectorAll('.phone-layout .chat-keys .key')].find(b => b.innerText === '^C').click()");
        page.wait(
            "another command",
            &say(".phone-layout", "echo back-$((20+3))"),
        );
        s.wait_text(&sh, "back-23");
        // Terminal, one tap; and the choice is this pane's from now on.
        page.js(&format!("{}.click()", face("Terminal")));
        page.wait(
            "the shell's terminal",
            &format!("!!document.querySelector('.phone-layout .xterm-rows') && {switch} === 'Chat Terminal*'"),
        );
        shoot(&page, "phone-layout-shell-terminal");

        // A pane by itself is the same pane.
        page.goto(&format!("#/s/alpha/p/{sh}"));
        page.wait(
            "the shell by itself, with the switch",
            "[...document.querySelectorAll('.phone-pane .chat-switch button')].length === 2",
        );
        page.js("[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Chat').click()");
        page.wait(
            "the shell's conversation by itself",
            "[...document.querySelectorAll('.phone-chat .chat-screen-text')].some(x => x.innerText.includes('back-23'))",
        );
        shoot(&page, "phone-shell-chat");
        page.goto(&format!("#/s/alpha/p/{codex}"));
        page.wait(
            "the Codex pane by itself",
            "[...document.querySelectorAll('.phone-chat .chat-text')].some(t => t.innerText.includes('counted what you asked'))",
        );
        shoot(&page, "phone-codex-found-session");
    }

    /// The same on a computer: each cell of a layout has both faces,
    /// whatever runs in it: "Chat" on a terminal's title bar, and the
    /// conversation's bar leads back.
    #[test]
    #[ignore = "merge tier: a browser"]
    fn on_a_computer_every_cell_of_a_layout_has_chat_and_terminal() {
        let Scene { s, codex, sh } = scene();
        let page = Page::open(&s, 1400, 900);
        page.goto("#/l/pair");
        let cell = |id: &str| format!(".chat-pane[data-pane=\"{id}\"]");
        // As they open: the agent as its conversation, the shell as its
        // terminal with "Chat" on its title bar ("Chat default for agents
        // terminal for terminals").
        page.wait(
            "the agent's cell as Chat and the shell's as its terminal",
            "document.querySelectorAll('.split .chat-pane').length === 1 && document.querySelectorAll('.split .face-btn').length === 1 && document.querySelector('.split .face-btn').innerText === 'Chat' && !!document.querySelector('.split .xterm-rows')",
        );
        shoot(&page, "desktop-layout-as-it-opens");
        page.js("document.querySelector('.split .face-btn').click()");
        let (c, t) = (cell(&codex), cell(&sh));
        let seen = page.wait(
            "both cells as conversations made from the screen",
            &format!("document.querySelectorAll('.split .chat-pane').length === 2 && !!document.querySelector('{c} .chat-src') && !!document.querySelector('{t} .chat-src') && [document.querySelector('{c} .chat-src').innerText.slice(0, 60), document.querySelector('{t} .chat-src').innerText, document.querySelectorAll('.split .chat-switch').length]"),
        );
        assert_eq!(
            seen,
            json!([
                "From the screen: Codex has not said which session it is in y",
                "From the screen: it runs a shell, not an agent",
                2
            ])
        );
        shoot(&page, "desktop-layout-codex-before-a-session");
        // A message from each cell reaches its own pane.
        page.wait(
            "a command from the shell's cell",
            &say(&t, "echo reed-$((40+2))"),
        );
        s.wait_text(&sh, "reed-42");
        page.wait(
            "a message from the Codex cell",
            &say(&c, "How many otters?"),
        );
        s.wait_text(&codex, "[fake] got: How many otters?");
        let seen = page.wait(
            "what each printed, in its own cell",
            &format!("[...document.querySelectorAll('{t} .chat-screen-text')].some(x => x.innerText.includes('reed-42')) && [...document.querySelectorAll('{c} .chat-text')].some(x => x.innerText.includes('counted what you asked')) && [[...document.querySelectorAll('{t} .chat-bubble')].map(x => x.innerText), [...document.querySelectorAll('{c} .chat-bubble')].map(x => x.innerText), !!document.querySelector('{c} .chat-src')]"),
        );
        assert_eq!(
            seen,
            json!([["echo reed-$((40+2))"], ["How many otters?"], false])
        );
        shoot(&page, "desktop-layout-codex-found-session");
        // Terminal is one click back, in the cell.
        page.js(&format!("[...document.querySelectorAll('{t} .chat-switch button')].find(b => b.innerText === 'Terminal').click()"));
        page.wait(
            "the shell's cell as its terminal again",
            "document.querySelectorAll('.split .chat-pane').length === 1 && document.querySelectorAll('.split .face-btn').length === 1 && !!document.querySelector('.split .xterm-rows')",
        );
        // A pane at full size, for the pictures.
        page.js("[...document.querySelectorAll('.split .face-btn')][0].click()");
        page.goto(&format!("#/s/alpha/p/{sh}"));
        page.wait(
            "the shell's conversation at full size",
            "[...document.querySelectorAll('.zoomed .chat-screen-text')].some(x => x.innerText.includes('reed-42'))",
        );
        shoot(&page, "desktop-shell-chat");
        page.goto(&format!("#/s/alpha/p/{codex}"));
        page.wait(
            "the Codex conversation at full size",
            "[...document.querySelectorAll('.zoomed .chat-text')].some(x => x.innerText.includes('counted what you asked'))",
        );
        shoot(&page, "desktop-codex-found-session");
    }
}
