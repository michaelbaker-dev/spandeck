//! A pane's look shows in whichever face the pane is in (DECISIONS.md,
//! "A pane's look is drawn in both of its faces"). The owner's report of 19
//! Nov 2026: "when I set the appearance of the left codex window and hit save
//! it didn't change, these should be tested to make sure they work in panes,
//! themes and layouts". The pane was a Codex agent's, shown as its
//! conversation, in a layout of two; the colour changed was the terminal's
//! background.
//!
//! Everything here is read from the real page in a real browser: the colour
//! the terminal draws its background and its text in, and the computed
//! colours of the conversation's own elements. What is covered, by level
//! (pane, layout, deck), face (Terminal, Chat) and kind of change (one
//! colour, a theme, one colour over a theme):
//!
//! | level  | change                | while editing | saved | reload | faces switched | taken away |
//! |--------|-----------------------|---------------|-------|--------|----------------|------------|
//! | pane   | one colour            | yes           | yes   | yes    | yes            | yes        |
//! | pane   | a theme, and a colour | yes           | yes   | yes    | yes            | yes        |
//! | layout | one colour            | yes           | yes   | yes    | (both faces)   |            |
//! | layout | a saved theme         | yes           | yes   | yes    | (both faces)   | yes        |
//! | deck   | a theme, and a colour | yes           | yes   | yes    | yes            | yes        |
//!
//! and a pane's own look over a layout's, a pane at full size, a pane's
//! picture on a station's page, and a phone. Part of `chat.rs` (the merge
//! tier):
//!
//!   cargo test -p spandeck --test chat look:: -- --ignored
use super::in_a_browser::{scene, shoot, Scene};
use super::*;
use common::browser::Page;

/// What a pane looks like on the page, whichever face it is in:
/// `[face, background, text]`, and for a conversation `low`: every piece of
/// it that is hard to read against what is drawn behind it. What is read
/// (the pane's name, what was said, the owner's bubble, what the pane
/// printed, the message box, Send) needs 4.5:1; the quiet words and small
/// buttons, which a theme sets in its palette's quiet colour (Solarized's is
/// 4.1:1 by design), need 3:1. Colours are as the browser worked them out,
/// as `#rrggbb`. `root` is the pane's frame (or, on a phone, the page's main
/// part).
const SEEN: &str = r#"((root) => {
    if (!root) return null;
    const ctx = (window.__ctx ||= document.createElement('canvas').getContext('2d', { willReadFrequently: true }));
    // [r, g, b, alpha] of any colour the browser gives.
    const rgba = (css) => { ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = '#000'; ctx.fillStyle = css; ctx.fillRect(0, 0, 1, 1); return [...ctx.getImageData(0, 0, 1, 1).data]; };
    const hex = (c) => '#' + c.slice(0, 3).map(x => x.toString(16).padStart(2, '0')).join('');
    // What is drawn behind an element: the nearest background that is not see-through.
    const behind = (e) => { for (; e; e = e.parentElement) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 250) return c; } return [0, 0, 0, 255]; };
    const lum = (c) => { const [r, g, b] = c.slice(0, 3).map(v => { const s = v / 255; return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const contrast = (a, b) => { const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); };
    const rows = root.querySelector('.xterm-rows');
    if (rows) return ['terminal', hex(behind(root.querySelector('.xterm-viewport'))), hex(rgba(getComputedStyle(rows).color))];
    const body = root.querySelector('.chat-body'), log = root.querySelector('.chat-log');
    if (!body || !log) return null;
    const low = [];
    const READ = ['.chat-bar .chat-title b', '.chat-text', '.chat-user .chat-bubble', '.chat-screen pre', '.chat-box', '.chat-send'];
    const QUIET = ['.chat-bar .chat-switch button', '.chat-quiet', '.chat-src', '.chat-act', '.chat-tools button'];
    for (const [sels, least] of [[READ, 4.5], [QUIET, 3]]) for (const sel of sels) {
        for (const e of root.querySelectorAll(sel)) {
            if (!e.getClientRects().length || !e.innerText.trim() && e.tagName !== 'TEXTAREA') continue;
            const c = contrast(rgba(getComputedStyle(e).color), behind(e));
            if (c < least) { low.push(`${sel} ${c.toFixed(2)}`); break; }
        }
    }
    // The words in the empty message box, where a look gave the pane colours of its own.
    const box = root.querySelector('.chat-box');
    if (box && box.closest('[data-themed]')) { const c = contrast(rgba(getComputedStyle(box, '::placeholder').color), behind(box)); if (c < 3) low.push(`placeholder ${c.toFixed(2)}`); }
    return ['chat', hex(behind(log)), hex(rgba(getComputedStyle(log).color)), low];
})"#;

/// The pane `id` of the layout or the single pane on a computer's screen.
fn frame(id: &str) -> String {
    format!("[...document.querySelectorAll('.pane')].find(p => (p.dataset.pane || p.paneTerm?.pane.id) === '{id}')")
}

/// The deck as it always was: Spandeck Dark, whose terminal and deck
/// colours are the same.
const DARK: [&str; 2] = ["#0f1318", "#d6dde6"];

/// A pane as its conversation on `bg`. (Its text is the terminal's, moved
/// as far as it takes to be read: `shows` checks that it can be.)
fn chat(bg: &str) -> Value {
    json!(["chat", bg, "*"])
}

/// A pane as its terminal, in these two colours.
fn terminal(bg: &str, text: &str) -> Value {
    json!(["terminal", bg, text])
}

struct Deck {
    s: Station,
    page: Page,
    codex: String,
    sh: String,
}

impl Deck {
    /// The owner's screen: a layout of two, a Codex agent on the left as its
    /// conversation (with something said in it) and a shell on the right as
    /// its terminal.
    fn open() -> Deck {
        let Scene { s, codex, sh } = scene();
        send(&s, &codex, "How many otters?");
        transcript_of(&s, &codex);
        let page = Page::open(&s, 1400, 900);
        page.goto("#/l/pair");
        let d = Deck { s, page, codex, sh };
        d.shows(
            "the deck as it always was",
            json!([["chat", DARK[0], DARK[1]], ["terminal", DARK[0], DARK[1]]]),
        );
        d
    }

    /// Wait until the two panes look like `want`: `[face, background,
    /// text]` of the left one and of the right one ("*" is any colour). Then
    /// nothing in a conversation is hard to read.
    fn shows(&self, what: &str, want: Value) {
        let both = format!(
            "[{SEEN}({}), {SEEN}({})]",
            frame(&self.codex),
            frame(&self.sh)
        );
        let seen = wait_for_or(
            what,
            20,
            || format!("wanted {want}\nthe page shows {}", self.page.js(&both)),
            || {
                let seen = self.page.js(&both);
                let same = (0..2).all(|i| {
                    (0..3).all(|k| {
                        seen[i][k].is_string()
                            && (want[i][k] == json!("*") || seen[i][k] == want[i][k])
                    })
                });
                same.then_some(seen)
            },
        );
        for pane in seen.as_array().unwrap() {
            if pane[0] == json!("chat") {
                assert_eq!(pane[3], json!([]), "{what}: text that is hard to read");
            }
        }
    }

    fn press(&self, sel: &str, text: &str) {
        self.page.wait(
            text,
            &format!(
                "(b => b ? (b.click(), true) : false)([...document.querySelectorAll({})].find(b => b.innerText.trim() === {}))",
                json!(sel),
                json!(text)
            ),
        );
    }

    /// Open the Appearance editor from a menu: `opener` (a JS expression)
    /// is the button that opens the menu. (A redraw closes a menu, so the
    /// button is pressed again until Appearance could be chosen.)
    fn appearance_from(&self, opener: &str) {
        self.page
            .wait("no dialog", "!document.querySelector('.dialog')");
        self.page.wait(
            "the menu, and Appearance in it",
            &format!("(i => i ? (i.click(), true) : (b => (b && b.click(), false))(!document.querySelector('.menu') && {opener}))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('Appearance')))"),
        );
        self.page
            .wait("the editor", "!!document.querySelector('#look-theme')");
    }

    /// The editor of a pane, from its own menu in the face it is in: "⋯" on
    /// a conversation's bar, "⋮" on a terminal's.
    fn pane_appearance(&self, id: &str) {
        let f = frame(id);
        self.appearance_from(&format!(
            "{f}?.querySelector('.chat-more, [aria-label=\"Pane menu\"]')"
        ));
    }

    fn layout_appearance(&self) {
        self.appearance_from(
            "document.querySelector('.station-main [aria-label=\"Layout menu\"]')",
        );
    }

    fn deck_appearance(&self) {
        self.page
            .wait("no dialog", "!document.querySelector('.dialog')");
        self.press(".topbar button", "Appearance");
        self.page
            .wait("the editor", "!!document.querySelector('#look-theme')");
    }

    /// Type a colour into a setting's box in the editor.
    fn colour(&self, key: &str, hex: &str) {
        self.page.wait(
            "the colour's box",
            &format!("(i => i ? (i.value = {}, i.dispatchEvent(new Event('input')), true) : false)(document.querySelector('.look-row[data-key=\"{key}\"] .look-hex'))", json!(hex)),
        );
    }

    /// Choose a theme in the editor, by the name it is listed under.
    fn theme(&self, name: &str) {
        self.page.wait(
            "the theme chosen",
            &format!("(s => (o => o ? (s.value = o.value, s.dispatchEvent(new Event('change')), true) : false)(s && [...s.options].find(o => o.textContent === {})))(document.querySelector('#look-theme'))", json!(name)),
        );
    }

    fn save(&self) {
        self.press(".look-foot button", "Save");
        self.closed();
    }

    fn closed(&self) {
        self.page
            .wait("the editor to close", "!document.querySelector('.dialog')");
    }

    /// Chat or Terminal, on the bar of a pane.
    fn face(&self, id: &str, to: &str) {
        let f = frame(id);
        self.page.wait(
            to,
            &format!("(b => b ? (b.click(), true) : false)([...({f}?.querySelectorAll('.chat-switch button, .face-btn') || [])].find(b => b.innerText === '{to}'))"),
        );
    }

    fn stored(&self) -> Value {
        self.s.call("look.get", json!({}))
    }

    fn pane_key(&self) -> String {
        format!("alpha/{}", self.codex)
    }
}

/// The owner's exact case. In a layout of two, the left pane is a Codex
/// agent shown as its conversation. Its Appearance is opened from its menu,
/// "Terminal colours → Background" is set to #006d8f, and Save is pressed.
/// The pane is that colour while the dialog is open, after Save, as its
/// terminal and as its conversation again, after a reload, at full size and
/// in its picture on the station's page; the pane beside it never changes;
/// and "Use the layout's" puts it back. Then the same with a theme for the
/// pane and one colour changed over it.
#[test]
#[ignore = "merge tier: a browser"]
fn a_panes_own_look_shows_in_its_conversation_and_in_its_terminal() {
    let d = Deck::open();
    let sh = terminal(DARK[0], DARK[1]);
    let blue = "#006d8f";

    d.pane_appearance(&d.codex);
    let says = d
        .page
        .js("document.querySelector('.look-form > p').innerText");
    assert!(
        says.as_str()
            .unwrap()
            .starts_with("Only counter: its terminal, its conversation (Chat) and its bar."),
        "{says}"
    );
    d.colour("term.background", blue);
    d.shows(
        "the change as it is made, before Save",
        json!([chat(blue), sh]),
    );
    assert_eq!(d.stored()["panes"], json!({}), "nothing is saved yet");
    d.save();
    let want = json!({ d.pane_key(): {"set": {"term.background": blue}} });
    wait_for("the pane's look to be stored", 10, || {
        (d.stored()["panes"] == want).then_some(())
    });
    d.shows("the pane after Save", json!([chat(blue), sh]));
    shoot(&d.page, "look-pane-own-blue-chat");

    // The same pane as its terminal, and as its conversation again.
    d.face(&d.codex, "Terminal");
    d.shows(
        "the pane as its terminal",
        json!([terminal(blue, DARK[1]), sh]),
    );
    shoot(&d.page, "look-pane-own-blue-terminal");
    d.face(&d.codex, "Chat");
    d.shows(
        "the pane as its conversation again",
        json!([chat(blue), sh]),
    );

    d.page.reload();
    d.shows("the pane after a reload", json!([chat(blue), sh]));

    // The pane by itself, at full size; and its picture among the station's panes.
    d.page.goto(&format!("#/s/alpha/p/{}", d.codex));
    d.page.wait(
        "the pane by itself, in its look",
        &format!(
            "(s => s && s[0] === 'chat' && s[1] === '{blue}' && s[3].length === 0)({SEEN}(document.querySelector('.zoomed .chat-pane')))"
        ),
    );
    d.page.goto("#/s/alpha");
    let thumbs = d.page.wait(
        "the panes' pictures",
        &format!(
            "(t => t.length === 2 && t.map(e => getComputedStyle(e).backgroundColor))(['{}', 'alpha/{}'].map(k => document.querySelector(`.thumb[data-key=\"${{k}}\"]`)).filter(Boolean))",
            d.pane_key(),
            d.sh
        ),
    );
    assert_eq!(
        thumbs,
        json!(["rgb(0, 109, 143)", "rgb(15, 19, 24)"]),
        "the picture of the pane with a look, then of the one without"
    );
    d.page.goto("#/l/pair");

    // "Use the layout's" takes the pane's own look away.
    d.pane_appearance(&d.codex);
    d.press(".look-foot button", "Use the layout's");
    d.closed();
    d.shows(
        "the pane in the layout's look again",
        json!([["chat", DARK[0], DARK[1]], sh]),
    );
    assert_eq!(d.stored()["panes"], json!({}));

    // A theme for the pane, then one colour changed over it.
    d.pane_appearance(&d.codex);
    d.theme("Solarized Light");
    d.shows("the theme as it is chosen", json!([chat("#fdf6e3"), sh]));
    let cream = "#fff8dc";
    d.colour("term.background", cream);
    d.shows("a colour changed over the theme", json!([chat(cream), sh]));
    d.save();
    d.shows(
        "the theme and the colour after Save",
        json!([chat(cream), sh]),
    );
    let want =
        json!({ d.pane_key(): {"theme": "solarized-light", "set": {"term.background": cream}} });
    assert_eq!(d.stored()["panes"], want);
    shoot(&d.page, "look-pane-light-theme-chat");
    d.face(&d.codex, "Terminal");
    d.shows(
        "the themed pane as its terminal",
        json!([terminal(cream, "#657b83"), sh]),
    );
    // (The face a pane was left in is this browser's, and is kept.)
    d.page.reload();
    d.shows(
        "the themed terminal after a reload",
        json!([terminal(cream, "#657b83"), sh]),
    );
    d.face(&d.codex, "Chat");
    d.shows(
        "the themed pane as its conversation again",
        json!([chat(cream), sh]),
    );
    d.pane_appearance(&d.codex);
    d.press(".look-foot button", "Use the layout's");
    d.closed();
    d.shows(
        "the deck as it always was, again",
        json!([["chat", DARK[0], DARK[1]], sh]),
    );
}

/// A layout's look, and the deck's: every pane shows it, the one shown as
/// its conversation and the one shown as its terminal. A pane's own look
/// lies over the layout's and comes off again; a theme saved with "Save as
/// theme…" is used by a layout; a built-in theme and a colour changed over
/// it are the deck's; and each is on the page while it is edited, after
/// Save and after a reload.
#[test]
#[ignore = "merge tier: a browser"]
fn a_layouts_look_and_the_decks_show_in_every_pane_in_either_face() {
    let d = Deck::open();

    // The layout: one colour.
    let plum = "#3b1d5e";
    d.layout_appearance();
    d.colour("term.background", plum);
    d.shows(
        "the layout's colour as it is made",
        json!([chat(plum), terminal(plum, DARK[1])]),
    );
    assert_eq!(d.stored()["layouts"], json!({}), "nothing is saved yet");
    d.save();
    d.shows(
        "the layout's colour after Save",
        json!([chat(plum), terminal(plum, DARK[1])]),
    );
    assert_eq!(
        d.stored()["layouts"],
        json!({"global:pair": {"set": {"term.background": plum}}})
    );
    d.page.reload();
    d.shows(
        "the layout's colour after a reload",
        json!([chat(plum), terminal(plum, DARK[1])]),
    );

    // A pane's own look lies over the layout's, in both of its faces, and
    // "Use the layout's" gives the pane the layout's again.
    let blue = "#006d8f";
    d.pane_appearance(&d.codex);
    d.colour("term.background", blue);
    d.save();
    d.shows(
        "the pane's own colour over the layout's",
        json!([chat(blue), terminal(plum, DARK[1])]),
    );
    d.face(&d.codex, "Terminal");
    d.shows(
        "the same as a terminal",
        json!([terminal(blue, DARK[1]), terminal(plum, DARK[1])]),
    );
    d.pane_appearance(&d.codex);
    d.press(".look-foot button", "Use the layout's");
    d.closed();
    d.shows(
        "the pane's terminal in the layout's look",
        json!([terminal(plum, DARK[1]), terminal(plum, DARK[1])]),
    );
    d.face(&d.codex, "Chat");
    d.shows(
        "the pane's conversation in the layout's look",
        json!([chat(plum), terminal(plum, DARK[1])]),
    );

    // The layout: a built-in theme with a colour changed, saved as a theme
    // of the owner's own, which the layout then uses.
    let slate = "#20303a";
    d.layout_appearance();
    d.theme("Nord");
    d.shows(
        "the layout's theme as it is chosen",
        json!([chat("#2e3440"), terminal("#2e3440", "#d8dee9")]),
    );
    d.colour("term.background", slate);
    d.press(".look-foot button", "Save as theme…");
    d.page
        .js("document.querySelector('.look-namer input').value = 'Mine'");
    d.press(".look-namer button", "Save theme");
    d.page.wait(
        "the new theme to be the one chosen",
        "document.querySelector('#look-theme').selectedOptions[0].textContent === 'Mine'",
    );
    d.save();
    d.shows(
        "the saved theme in both panes",
        json!([chat(slate), terminal(slate, "#d8dee9")]),
    );
    let doc = d.stored();
    assert_eq!(doc["themes"][0]["name"], json!("Mine"), "{doc}");
    assert_eq!(
        doc["layouts"],
        json!({"global:pair": {"theme": doc["themes"][0]["id"]}}),
        "{doc}"
    );
    d.page.reload();
    d.shows(
        "the saved theme after a reload",
        json!([chat(slate), terminal(slate, "#d8dee9")]),
    );
    d.layout_appearance();
    d.press(".look-foot button", "Use the deck's");
    d.closed();
    d.shows(
        "the layout in the deck's look again",
        json!([["chat", DARK[0], DARK[1]], terminal(DARK[0], DARK[1])]),
    );

    // The deck: a light built-in theme, whose terminal is white on a grey
    // page, then one colour over it.
    d.deck_appearance();
    d.theme("Spandeck Light");
    d.shows(
        "the deck's theme as it is chosen",
        json!([chat("#ffffff"), terminal("#ffffff", "#1f2933")]),
    );
    d.save();
    d.shows(
        "the deck's theme after Save",
        json!([chat("#ffffff"), terminal("#ffffff", "#1f2933")]),
    );
    shoot(&d.page, "look-deck-light-theme-chat");
    let ivory = "#fffff0";
    d.deck_appearance();
    d.colour("term.background", ivory);
    d.shows(
        "a colour over the deck's theme as it is made",
        json!([chat(ivory), terminal(ivory, "#1f2933")]),
    );
    d.save();
    assert_eq!(
        d.stored()["deck"],
        json!({"theme": "spandeck-light", "set": {"term.background": ivory}})
    );
    d.page.reload();
    d.shows(
        "the deck's look after a reload",
        json!([chat(ivory), terminal(ivory, "#1f2933")]),
    );
    // Each pane in its other face.
    d.face(&d.codex, "Terminal");
    d.face(&d.sh, "Chat");
    d.shows(
        "the deck's look with the faces changed over",
        json!([terminal(ivory, "#1f2933"), chat(ivory)]),
    );
    d.face(&d.codex, "Chat");
    d.face(&d.sh, "Terminal");
    // Back to the theme the deck came with.
    d.deck_appearance();
    d.theme("Spandeck Dark");
    d.save();
    d.shows(
        "the deck as it always was, again",
        json!([["chat", DARK[0], DARK[1]], terminal(DARK[0], DARK[1])]),
    );
}

/// At a phone's width, where Appearance is a sheet with no colours in it: a
/// look given to a pane on a computer shows in the pane's conversation on
/// the phone, by itself and as a pane of its layout, and in its terminal
/// there; and it shows on a page that was already open when it was saved.
#[test]
#[ignore = "merge tier: a browser"]
fn on_a_phone_a_panes_saved_look_shows_in_its_conversation() {
    let Scene { s, codex, sh: _sh } = scene();
    send(&s, &codex, "How many otters?");
    transcript_of(&s, &codex);
    let page = Page::open(&s, 390, 844);
    page.phone_screen();
    let seen = |root: &str| format!("{SEEN}(document.querySelector('{root}'))");
    let shows = |what: &str, root: &str, face: &str, bg: &str| {
        let seen = page.wait(
            what,
            &format!(
                "(s => s && s[0] === '{face}' && s[1] === '{bg}' && s)({})",
                seen(root)
            ),
        );
        if face == "chat" {
            assert_eq!(seen[3], json!([]), "{what}: text that is hard to read");
        }
    };
    page.goto(&format!("#/s/alpha/p/{codex}"));
    shows(
        "the conversation as it always was",
        ".phone-chat",
        "chat",
        DARK[0],
    );

    // Saved elsewhere while this page is open.
    let blue = "#006d8f";
    s.call(
        "look.set",
        json!({"level": "pane", "key": format!("alpha/{codex}"), "choice": {"set": {"term.background": blue}}}),
    );
    shows("the look saved elsewhere", ".phone-chat", "chat", blue);
    shoot(&page, "look-phone-pane-own-blue-chat");
    page.reload();
    shows("the look after a reload", ".phone-chat", "chat", blue);

    // The same pane's terminal on the phone, and its conversation again.
    let face = |to: &str| {
        page.wait(
            to,
            &format!("(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.chat-bar .chat-switch button')].find(b => b.innerText === '{to}'))"),
        );
    };
    face("Terminal");
    shows("the pane's terminal", ".phone-pane", "terminal", blue);
    face("Chat");
    shows("the pane's conversation again", ".phone-chat", "chat", blue);

    // As a pane of its layout.
    page.goto("#/l/pair");
    shows("the pane in its layout", ".phone-layout", "chat", blue);
}
