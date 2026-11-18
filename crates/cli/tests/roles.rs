//! The role library (DECISIONS.md, "Roles for agents", 18 Nov 2026): the
//! owner's request that an agent is started with a defined role whose
//! prompt the owner can view and change. These are that feature's
//! acceptance criteria; the document's own rules are tested beside it
//! (`core/src/roledoc.rs`), and who may call what in `rpc.rs`.
mod common;
use std::process::Stdio;

use common::*;
use serde_json::{json, Value};

const HEADER: &str = "Your role in this project, from the owner:";

/// The role messages the owner has sent, oldest first: (to, body).
fn sent(s: &Station) -> Vec<(String, String)> {
    let mut all: Vec<(u64, String, String)> = s
        .call("msg.recent", json!({"limit": 200}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["body"].as_str().is_some_and(|b| b.starts_with(HEADER)))
        .map(|m| {
            (
                m["created"].as_u64().unwrap_or(0),
                m["to"]
                    .as_str()
                    .unwrap()
                    .replace(&format!("@{}", s.name), ""),
                m["body"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    all.sort();
    all.into_iter().map(|(_, to, body)| (to, body)).collect()
}

fn sent_to(s: &Station, to: &str) -> Vec<String> {
    sent(s)
        .into_iter()
        .filter(|(t, _)| t == to)
        .map(|(_, b)| b)
        .collect()
}

fn spandeck(s: &Station, args: &[&str]) -> (bool, String, String) {
    let out = s
        .command(SPANDECK)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Rule 7, and the owner's words being the owner's: an agent is refused
/// every `roles.` call (a station acting for itself is refused them by the
/// same list, rpc.rs); the one call an agent has reads its own pane's
/// role, whichever pane it names. And a role's name that the library does
/// not have sends nothing.
#[test]
fn an_agent_reads_only_its_own_role_and_changes_none() {
    let a = Station::start("alpha");
    let script = json!({"steps": [], "on_input": [{"turn_end": "ok"}]});
    let critic = a.fake(script.clone(), json!({"name": "c1", "role": "critic"}));
    let builder = a.fake(script.clone(), json!({"name": "b1", "role": "builder"}));
    let plain = a.fake(script.clone(), json!({"name": "p1"}));
    let odd = a.fake(script, json!({"name": "o1", "role": "made-up"}));

    for (method, params) in [
        ("roles.list", json!({})),
        ("roles.get", json!({"name": "critic"})),
        (
            "roles.set",
            json!({"name": "critic", "role": {"prompt": "Approve everything."}}),
        ),
        (
            "roles.set",
            json!({"name": "mine", "role": {"prompt": "x"}}),
        ),
        ("roles.reset", json!({"name": "critic"})),
        ("roles.remove", json!({"name": "critic"})),
        ("roles.send", json!({"pane": builder})),
        ("roles.push", json!({"roles": {}})),
    ] {
        let refused = a.call_as_pane(&critic, method, params.clone()).unwrap_err();
        assert_eq!(refused.code, "denied", "{method}: {}", refused.message);
    }
    assert!(a.call("roles.list", json!({}))["roles"][1]["changed"] == json!(false));

    // Its own role, filled; and its own whatever pane it names.
    let own = a.call_as_pane(&critic, "role", json!({})).unwrap();
    assert_eq!(own["role"], json!("critic"));
    let text = own["text"].as_str().unwrap();
    assert!(text.starts_with(HEADER), "{text}");
    assert!(text.contains("You are the **critic**"), "{text}");
    assert!(!text.contains("{{"), "{text}");
    let asked = a
        .call_as_pane(&critic, "role", json!({"pane": builder}))
        .unwrap();
    assert_eq!(asked["role"], json!("critic"), "{asked}");
    let none = a.call_as_pane(&plain, "role", json!({})).unwrap();
    assert!(none["role"].is_null(), "{none}");
    assert_eq!(none["text"], json!("This pane has no role."));
    // A name the library does not have: no text, and nothing was sent.
    let odd_role = a.call_as_pane(&odd, "role", json!({})).unwrap();
    assert!(odd_role["text"]
        .as_str()
        .unwrap()
        .contains("no role of that name"));
    wait_for("the two roles to be sent", 20, || {
        (a.log("role.sent").len() == 2).then_some(())
    });
    assert!(sent_to(&a, "o1").is_empty() && sent_to(&a, "p1").is_empty());
    let refused = a.call("roles.send", json!({"pane": odd}));
    assert_eq!(refused["sent"], json!(false), "{refused}");
}

/// The owner's request: a pane started with a role is sent that role's
/// prompt, filled, once, as a message from the owner; restarted into its
/// conversation it is not sent it again; `roles.send` sends it again; the
/// ship's log says which text was sent. The owner's own text is what is
/// sent once the owner changed it, by the command line. And the bug of 18
/// Nov 2026 stays fixed for this message: it is not typed into the
/// program's first question.
#[test]
#[ignore = "merge tier: waits five seconds for a nudge that must not come"]
fn a_pane_started_with_a_role_is_sent_it_once_and_again_when_asked() {
    let s = Station::start("alpha");
    let dir = s.root.join("fresh");
    std::fs::create_dir_all(&dir).unwrap();

    // A program whose first screen is its own question.
    let script = s.script_file(&json!({
        "trust_screen": {"as": "claude"}, "steps": [], "on_input": [{"turn_end": "ok"}],
    }));
    let early = s.call(
        "pane.spawn",
        json!({"agent": "claude", "program": FAKE, "args": ["--script", script],
               "cwd": dir, "name": "early", "role": "security"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for("the role to be stored for the pane", 20, || {
        (sent_to(&s, "early").len() == 1).then_some(())
    });
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(s.log("msg.nudged").is_empty(), "{:?}", s.log("msg.nudged"));
    let p = s.pane(&early);
    assert_eq!(p["exited"], json!(false), "{p}");
    assert!(p["trust"]["asks"].is_object(), "{p}");
    let body = &sent_to(&s, "early")[0];
    assert!(
        body.contains("one security review of `fresh`")
            && body.contains("You are working alone in this project.")
            && body.contains("Budget: One review, about 30 minutes, at most seven findings."),
        "{body}"
    );

    // By the command line, with the program the role names when none is.
    let (ok, _, err) = spandeck(
        &s,
        &[
            "roles",
            "edit",
            "user",
            "--agent",
            "fake",
            "--budget",
            "Five minutes.",
        ],
    );
    assert!(ok, "{err}");
    let (ok, id, err) = spandeck(&s, &["pane", "new", "--role", "user", "--name", "u1"]);
    assert!(ok, "{err}");
    let id = id.trim().to_string();
    assert_eq!(s.pane(&id)["agent"], json!("fake"));
    assert_eq!(s.pane(&id)["role"], json!("user"));
    let first = wait_for("u1's role", 20, || sent_to(&s, "u1").pop());
    assert!(
        first.contains("Five minutes.") && first.contains("You are the **user**"),
        "{first}"
    );
    let (ok, _, err) = spandeck(&s, &["pane", "new", "--role", "no-such", "--agent", "fake"]);
    assert!(!ok && err.contains("spandeck roles"), "{err}");

    // Started again into its conversation: not sent again. Asked: again.
    s.call("pane.restart", json!({"pane": id}));
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(sent_to(&s, "u1").len(), 1);
    let (ok, out, err) = spandeck(&s, &["roles", "send", "u1"]);
    assert!(
        ok && out.contains("Sent u1 its role (user, the owner's text)"),
        "{out}{err}"
    );
    assert_eq!(sent_to(&s, "u1").len(), 2);
    let log = s.log("role.sent");
    let of = |pane: &str| -> Vec<Value> {
        log.iter()
            .filter(|e| e["pane"] == json!(pane))
            .map(|e| e["data"].clone())
            .collect()
    };
    assert_eq!(of(&early)[0]["text"], json!("built-in"), "{log:?}");
    assert_eq!(of(&early)[0]["role"], json!("security"));
    assert_eq!(of(&id).len(), 2, "{log:?}");
    assert_eq!(of(&id)[0]["text"], json!("owner's"));
    let again: Vec<bool> = of(&id).iter().map(|d| d["again"] == json!(true)).collect();
    assert!(again.contains(&true) && again.contains(&false), "{log:?}");
}

/// The pair recipe uses the library, and takes a third pane: with `--user`
/// there are three panes, each sent its own role's prompt, naming the
/// other two, with the recipe's line after it, in one message each; the
/// budget the owner named is in the process file and in each role.
#[test]
#[ignore = "merge tier: three panes and the command line"]
fn the_pair_recipe_with_a_user_sends_three_roles() {
    let s = Station::start("alpha");
    let dir = s.root.join("shop");
    let (ok, out, err) = spandeck(
        &s,
        &[
            "--json",
            "project",
            "new",
            "--recipe",
            "pair",
            "--path",
            dir.to_str().unwrap(),
            "--builder",
            "fake",
            "--critic",
            "fake",
            "--user",
            "--budget",
            "30",
            "--brief",
            "A shop for one kind of tea.",
        ],
    );
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["recipe"]["roles"], json!("sent"), "{v}");
    let mut names: Vec<&str> = v["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["builder", "critic", "user"]);
    for p in v["panes"].as_array().unwrap() {
        assert_eq!(p["role"], p["name"], "{p}");
    }

    let process = std::fs::read_to_string(dir.join("PROCESS.md")).unwrap();
    for want in [
        "**user** (Fake agent)",
        "**30 minutes a stage**",
        "asks the **user** pane to look (about 15 minutes)",
        "one acceptance list",
        "begins with using the product",
    ] {
        assert!(process.contains(want), "{want}: {process}");
    }
    assert!(!process.contains("{{"), "{process}");

    let all = sent(&s);
    assert_eq!(all.len(), 3, "{all:?}");
    let of = |pane: &str| sent_to(&s, &format!("shop/{pane}")).pop().unwrap();
    let builder = of("builder");
    assert!(
        builder.starts_with(&format!("{HEADER}\n\n# Role: builder")),
        "{builder}"
    );
    assert!(builder.contains("You are the **builder** in the project `shop`"));
    assert!(
        builder.contains("The other agents here: the pane `critic` (role: critic, Fake agent) and the pane `user` (role: user, Fake agent). Reach them with spandeck's messages."),
        "{builder}"
    );
    assert!(builder.contains("30 minutes a stage."), "{builder}");
    assert!(
        builder.ends_with("Read BRIEF.md and PROCESS.md in this folder, then start stage 0. Work with the other agents as PROCESS.md says, without waiting for me."),
        "{builder}"
    );
    let critic = of("critic");
    assert!(
        critic.contains("# Role: critic")
            && critic
                .contains("the pane `builder` (role: builder, Fake agent) and the pane `user`"),
        "{critic}"
    );
    let user = of("user");
    assert!(
        user.contains("# Role: user") && user.contains("About 15 minutes a look"),
        "{user}"
    );
    assert!(
        user.contains("the pane `builder` (role: builder, Fake agent) and the pane `critic`"),
        "{user}"
    );
    assert!(
        user.ends_with("without waiting for me.")
            && user.contains("Then wait: the builder asks you to look"),
        "{user}"
    );
    assert!(!builder.contains("{{") && !critic.contains("{{") && !user.contains("{{"));

    // Opened again, and run again: nobody is told twice.
    let opened = s.call("project.open", json!({"name": "shop"}));
    assert_eq!(opened["recipe"]["roles"], json!("sent before"), "{opened}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(sent(&s).len(), 3);
}

/// One library for the fleet: a role the owner changed at one station is
/// the same at the other, marked changed there too; a role added at the
/// other comes back; a reset travels. And the command line shows it.
#[test]
#[ignore = "merge tier: two stations converge"]
fn two_stations_converge_on_an_edited_role() {
    let (a, b) = Station::pair();
    let file = a.root.join("critic.md");
    std::fs::write(&file, "# Critic\nIn {{project}}: be kind. {{others}}\n").unwrap();
    let (ok, out, err) = spandeck(
        &a,
        &["roles", "edit", "critic", "--file", file.to_str().unwrap()],
    );
    assert!(
        ok && out.contains("built-in role changed by you"),
        "{out}{err}"
    );
    let at = |s: &Station, name: &str| {
        s.try_call("roles.get", json!({"name": name}))
            .ok()
            .map(|v| v["role"].clone())
    };
    wait_for("bravo to have alpha's critic", 30, || {
        at(&b, "critic").filter(|r| r["changed"] == json!(true))
    });
    let theirs = at(&b, "critic").unwrap();
    assert_eq!(
        theirs["prompt"],
        json!("# Critic\nIn {{project}}: be kind. {{others}}\n")
    );
    assert!(theirs["builtin_role"]["prompt"]
        .as_str()
        .unwrap()
        .starts_with("# Role: critic"));
    assert!(
        theirs["changed_by"].as_str().unwrap().ends_with("at alpha"),
        "{theirs}"
    );

    // A save made to the copy as it was before is refused, and says so.
    let lost = b.call(
        "roles.set",
        json!({"name": "critic", "role": {"prompt": "mine"}, "was": 0}),
    );
    assert_eq!(lost["saved"], json!(false), "{lost}");
    assert!(lost["why"].as_str().unwrap().contains("at alpha"), "{lost}");
    assert_eq!(at(&b, "critic").unwrap()["prompt"], theirs["prompt"]);

    let (ok, _, err) = spandeck(
        &b,
        &[
            "roles",
            "new",
            "copy-editor",
            "--copy",
            "critic",
            "--title",
            "Copy editor",
            "--kind",
            "visit",
        ],
    );
    assert!(ok, "{err}");
    wait_for("alpha to have bravo's role", 30, || at(&a, "copy-editor"));
    let (_, list, _) = spandeck(&a, &["roles"]);
    let line = |name: &str| {
        list.lines()
            .find(|l| l.starts_with(name))
            .unwrap_or("")
            .to_string()
    };
    assert!(
        line("critic").contains("changed") && line("critic").contains("on the team"),
        "{list}"
    );
    assert!(
        line("copy-editor").contains("yours") && line("copy-editor").contains("a short visit"),
        "{list}"
    );
    assert!(!line("builder").contains("changed"), "{list}");
    let (_, shown, _) = spandeck(
        &a,
        &[
            "roles",
            "show",
            "copy-editor",
            "--filled",
            "--project",
            "none",
        ],
    );
    assert_eq!(
        shown,
        "# Critic\nIn none: be kind. You are working alone in this project.\n"
    );

    let (ok, _, err) = spandeck(&b, &["roles", "reset", "critic"]);
    assert!(ok, "{err}");
    wait_for("alpha's critic to be the built-in again", 30, || {
        at(&a, "critic").filter(|r| r["changed"] == json!(false))
    });
    let (ok, _, err) = spandeck(&a, &["roles", "remove", "critic"]);
    assert!(!ok && err.contains("cannot be removed"), "{err}");
    let (ok, _, err) = spandeck(&a, &["roles", "remove", "copy-editor"]);
    assert!(ok, "{err}");
    wait_for("bravo to have lost the role", 30, || {
        at(&b, "copy-editor").is_none().then_some(())
    });
}

/// The Control Deck's Roles dialog, in a real browser. With
/// `SPANDECK_SHOTS` set, pictures are written there:
/// `SPANDECK_SHOTS=/some/dir cargo test --test roles -- --ignored`
#[cfg(unix)]
mod deck {
    use super::*;
    use common::browser::Page;
    use std::path::PathBuf;

    #[derive(Clone, Copy, PartialEq)]
    enum Size {
        Phone,
        Small,
        Desktop,
    }

    fn open(s: &Station, size: Size) -> Page {
        match size {
            Size::Desktop => Page::open(s, 1200, 800),
            Size::Phone => {
                let p = Page::open(s, 390, 844);
                p.phone_screen();
                p
            }
            Size::Small => {
                let p = Page::open(s, 390, 700);
                p.protocol(
                    "Emulation.setDeviceMetricsOverride",
                    json!({"width": 320, "height": 640, "deviceScaleFactor": 1, "mobile": false}),
                );
                p.wait("a small phone's screen", "innerWidth === 320");
                p
            }
        }
    }

    fn shot(page: &Page, name: &str) {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        if let Some(out) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) {
            std::fs::create_dir_all(&out).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(350));
            let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
            std::fs::write(out.join(format!("roles-{name}.png")), png).unwrap();
        }
    }

    /// Open the Roles dialog the way the owner does: the top bar's button
    /// on a desktop, the top bar's menu on a phone.
    fn open_roles(page: &Page, phone: bool) {
        if phone {
            page.wait(
                "Roles in the top bar's menu",
                "(i => i ? (i.click(), true) : (document.querySelector('.top-more')?.click(), false))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('Roles')))",
            );
        } else {
            page.wait(
                "the Roles button, beside Tools",
                "(b => b && b.previousElementSibling?.classList.contains('tools-btn') ? (b.click(), true) : false)(document.querySelector('.topbar .roles-btn'))",
            );
        }
        page.wait(
            "the seven roles",
            "document.querySelectorAll('.roles-panel .ro-row').length >= 7",
        );
    }

    fn click(page: &Page, what: &str, sel: &str) {
        page.wait(
            what,
            &format!(
                "(b => b && !b.disabled ? (b.click(), true) : false)(document.querySelector({}))",
                json!(sel)
            ),
        );
    }

    /// Put text in a box as typing does: the box's own value, then `input`.
    fn set(page: &Page, sel: &str, value: &str) {
        page.js(&format!(
            "(el => {{ el.focus(); el.value = {}; el.dispatchEvent(new Event('input', {{bubbles: true}})); el.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})(document.querySelector({}))",
            json!(value),
            json!(sel)
        ));
    }

    /// 44 px targets and nothing off the side, in every dialog that is open.
    const FITS: &str = "[...document.querySelectorAll('.dialog')].every(d => { const b = d.querySelector('.dialog-body'); return b.scrollWidth <= b.clientWidth && d.getBoundingClientRect().right <= innerWidth + 0.5 && [...d.querySelectorAll('*')].every(x => !x.offsetParent || x.getBoundingClientRect().right <= innerWidth + 0.5) && [...d.querySelectorAll('button, summary, select, input:not([type=checkbox])')].filter(x => x.offsetParent).every(x => x.getBoundingClientRect().height >= 43.5); }) && document.documentElement.scrollWidth <= innerWidth";
    const MISFITS: &str = "JSON.stringify([innerWidth, document.documentElement.scrollWidth, ...[...document.querySelectorAll('.dialog *')].filter(x => x.offsetParent && (x.getBoundingClientRect().right > innerWidth + 0.5 || (x.matches('button, summary, select, input:not([type=checkbox])') && x.getBoundingClientRect().height < 43.5))).slice(0, 8).map(x => [x.tagName, x.className, x.innerText.slice(0, 40), Math.round(x.getBoundingClientRect().right), Math.round(x.getBoundingClientRect().height)])])";

    fn fits(page: &Page, size: Size, what: &str) {
        if size != Size::Desktop {
            assert_eq!(page.js(FITS), json!(true), "{what}: {}", page.js(MISFITS));
        }
    }

    /// The owner's request, start to end: see the roles, read one's whole
    /// prompt, change it and save, see "changed by you", reset it, make a
    /// role by copying one, start a pane with a role and see it sent. What
    /// is typed and not saved survives the dialog being drawn again and a
    /// save that lost to a newer copy, and the dialog asks before it is
    /// closed over it.
    fn the_owners_request(size: Size) {
        let phone = size != Size::Desktop;
        let tag = match size {
            Size::Phone => "phone",
            Size::Small => "320",
            Size::Desktop => "desktop",
        };
        let scripts = tempfile::tempdir().unwrap();
        let script = scripts.path().join("idle.json");
        std::fs::write(
            &script,
            json!({"steps": [], "on_input": [{"turn_end": "ok"}]}).to_string(),
        )
        .unwrap();
        let s = Station::start_with(
            "alpha",
            &format!(
                "[agents.claude]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n",
                script.display()
            ),
            &[],
        );
        let builtin = s.call("roles.get", json!({"name": "critic"}))["role"]["prompt"]
            .as_str()
            .unwrap()
            .to_string();
        let page = open(&s, size);
        open_roles(&page, phone);

        // The list: title, name, kind, summary; nothing changed yet.
        let rows = page.js("JSON.stringify([...document.querySelectorAll('.ro-row')].map(r => [r.dataset.role, r.querySelector('b').innerText, r.querySelector('.ro-kind').innerText, !!r.querySelector('.ro-changed'), r.querySelector('.ro-summary').innerText.length > 20]))");
        let rows: Value = serde_json::from_str(rows.as_str().unwrap()).unwrap();
        assert_eq!(
            rows[0],
            json!(["builder", "Builder", "on the team", false, true])
        );
        assert_eq!(
            rows[1],
            json!(["critic", "Critic", "on the team", false, true])
        );
        assert_eq!(
            rows[6],
            json!([
                "security",
                "Security reviewer",
                "a short visit",
                false,
                true
            ])
        );
        fits(&page, size, "the list");
        shot(&page, &format!("{tag}-1-list"));

        // One role: its whole prompt, as text, wrapped, with its defaults.
        click(&page, "the critic", ".ro-row[data-role=critic]");
        page.wait(
            "the critic's prompt",
            "!!document.querySelector('.ro-role-view .ro-prompt')",
        );
        assert_eq!(
            page.js("document.querySelector('.ro-role-view .ro-prompt').textContent"),
            json!(builtin),
            "the whole prompt, as it is stored"
        );
        assert_eq!(
            page.js("(p => p.children.length === 0 && getComputedStyle(p).whiteSpace === 'pre-wrap' && p.scrollWidth <= p.clientWidth)(document.querySelector('.ro-role-view .ro-prompt'))"),
            json!(true),
            "text and never HTML, wrapped, not wider than its box"
        );
        assert_eq!(
            page.js("(d => d.includes('Budget') && d.includes('One round a stage unless there is a blocker'))(document.querySelector('.ro-defaults').innerText)"),
            json!(true)
        );
        fits(&page, size, "one role");
        shot(&page, &format!("{tag}-2-role"));

        // Edit: the prompt in a large box, the count, the placeholders.
        click(&page, "Edit", ".ro-edit");
        page.wait(
            "the editor",
            "document.querySelector('.ro-f-prompt')?.value.length > 100",
        );
        assert_eq!(
            page.js("document.querySelector('.ro-f-prompt').value"),
            json!(builtin)
        );
        let n = builtin.len().to_string();
        assert_eq!(
            page.js("document.querySelector('.ro-count').innerText"),
            json!(format!(
                "{},{} of 16,384 bytes",
                &n[..n.len() - 3],
                &n[n.len() - 3..]
            ))
        );
        assert_eq!(
            page.js("(t => ['{{project}}', '{{others}}', '{{budget}}', 'the other agent panes'].every(w => t.includes(w)))(document.querySelector('.ro-places').textContent)"),
            json!(true)
        );
        assert_eq!(
            page.js("!document.querySelector('.ro-reset') && !!document.querySelector('.ro-show-builtin') && !!document.querySelector('.ro-save') && !!document.querySelector('.ro-cancel')"),
            json!(true),
            "nothing to reset until it is changed"
        );
        let mine = format!("{builtin}\nAlso read the changelog. <b>not html</b> {{{{mine}}}}\n");
        set(&page, ".ro-f-prompt", &mine);
        page.js("document.querySelector('.ro-f-prompt').dataset.same = 'yes'");
        page.wait(
            "it says it is not saved",
            "document.querySelector('.ro-state').innerText === 'Not saved yet' && document.querySelector('.ro-odd').innerText.includes('{{mine}}')",
        );
        fits(&page, size, "the editor");
        shot(&page, &format!("{tag}-3-editor"));

        // The dialog is drawn again (another role changes somewhere, and
        // the page hears of a pane): the box and its words are the same.
        let other = s.fake(json!({"steps": []}), json!({"name": "other"}));
        s.call(
            "roles.set",
            json!({"name": "security", "role": {"budget": "Ten minutes."}}),
        );
        page.redraw(&s, &other);
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert_eq!(
            page.js("document.querySelector('.ro-f-prompt').value"),
            json!(mine)
        );
        assert_eq!(
            page.js("document.querySelector('.ro-f-prompt').dataset.same"),
            json!("yes"),
            "the same box, not another with the same words"
        );

        // Closing over it asks first, and keeps it.
        page.js("document.querySelector('.roles-sheet .dialog-head .icon-btn').click()");
        page.wait(
            "the question",
            "document.querySelector('.ro-ask')?.innerText.includes('Discard what you changed?')",
        );
        fits(&page, size, "the question");
        click(&page, "Keep editing", ".ro-keep");
        assert_eq!(
            page.js("!document.querySelector('.ro-ask') && document.querySelector('.ro-f-prompt').value.length"),
            json!(mine.chars().count())
        );

        // The same role is changed somewhere else first: the save says so,
        // and the owner's text is still in the box.
        s.call(
            "roles.set",
            json!({"name": "critic", "role": {"budget": "Theirs."}}),
        );
        page.wait(
            "it says the role moved",
            "!!document.querySelector('.ro-moved')",
        );
        click(&page, "Save", ".ro-save");
        page.wait(
            "not saved, and why",
            "(c => c && c.innerText.includes('Not saved: this role was changed by you at alpha after you opened it') && c.innerText.includes('Your text is still in the box'))(document.querySelector('.ro-conflict'))",
        );
        assert_eq!(
            page.js("document.querySelector('.ro-f-prompt').value"),
            json!(mine)
        );
        assert_eq!(
            s.call("roles.get", json!({"name": "critic"}))["role"]["prompt"],
            json!(builtin)
        );
        page.js("document.querySelector('.ro-conflict').scrollIntoView({block: 'center'})");
        fits(&page, size, "the save that lost");
        shot(&page, &format!("{tag}-4-newer"));
        click(&page, "Save mine over it", ".ro-over");
        page.wait(
            "saved, and said",
            "document.querySelector('.ro-role-view .ro-said')?.innerText.startsWith('Saved on alpha') && !!document.querySelector('.ro-role-view .ro-changed')",
        );
        let saved = s.call("roles.get", json!({"name": "critic"}))["role"].clone();
        assert_eq!(saved["prompt"], json!(mine));
        assert_eq!(saved["changed"], json!(true));
        // What was typed is shown as text.
        assert_eq!(
            page.js("(p => p.children.length === 0 && p.textContent.includes('<b>not html</b>'))(document.querySelector('.ro-role-view .ro-prompt'))"),
            json!(true)
        );
        assert_eq!(
            page.js("document.querySelector('.ro-builtin .ro-prompt').textContent"),
            json!(builtin),
            "the built-in text is still there to compare"
        );
        click(&page, "back to the list", ".ro-back");
        page.wait(
            "changed by you, in the list",
            "document.querySelector('.ro-row[data-role=critic] .ro-changed')?.innerText === 'changed by you' && !document.querySelector('.ro-row[data-role=builder] .ro-changed')",
        );
        shot(&page, &format!("{tag}-5-changed"));

        // Reset: asked once, then the built-in text again.
        click(&page, "the critic", ".ro-row[data-role=critic]");
        click(&page, "Reset", ".ro-role-view .ro-reset");
        page.wait(
            "the question",
            "document.querySelector('.ro-ask')?.innerText.includes('Go back to the built-in text of critic?')",
        );
        click(&page, "Reset, said twice", ".ro-discard");
        page.wait(
            "the built-in text again",
            "document.querySelector('.ro-said')?.innerText.includes('built-in text again') && !document.querySelector('.ro-role-view .ro-changed')",
        );
        assert_eq!(
            page.js("document.querySelector('.ro-role-view .ro-prompt').textContent"),
            json!(builtin)
        );
        assert_eq!(
            s.call("roles.get", json!({"name": "critic"}))["role"]["changed"],
            json!(false)
        );

        // A new role, copied from the critic.
        click(&page, "Copy to a new role", ".ro-copy");
        page.wait(
            "the new role's form, with the critic's text",
            "document.querySelector('.ro-f-copy')?.value === 'critic' && document.querySelector('.ro-f-prompt').value.startsWith('# Role: critic') && document.querySelector('.ro-f-name').value === ''",
        );
        click(&page, "Save, with no name", ".ro-save");
        page.wait(
            "it asks for a name",
            "document.querySelector('.ro-said')?.innerText.includes('Give the role a name')",
        );
        set(&page, ".ro-f-name", "copy-editor");
        set(&page, ".ro-f-title", "Copy editor");
        set(&page, ".ro-f-summary", "Reads every word a user will see.");
        set(&page, ".ro-f-kind", "visit");
        fits(&page, size, "a new role");
        page.js("document.querySelector('.dialog-body:has(.roles-panel)').scrollTop = 0");
        shot(&page, &format!("{tag}-6-new"));
        click(&page, "Save", ".ro-save");
        page.wait(
            "the new role, marked as the owner's",
            "document.querySelector('.ro-role-view')?.dataset.role === 'copy-editor' && !!document.querySelector('.ro-role-view .ro-own') && document.querySelector('.ro-role-view .ro-kind').innerText === 'a short visit'",
        );
        let made = s.call("roles.get", json!({"name": "copy-editor"}))["role"].clone();
        assert_eq!(made["prompt"], json!(builtin));
        assert_eq!(made["source"], json!("own"));
        assert_eq!(made["title"], json!("Copy editor"));

        // Closed by its own control.
        fits(&page, size, "the new role, saved");
        click(&page, "Close", ".ro-close");
        page.wait("the dialog gone", "!document.querySelector('.roles-panel')");

        // Where a role is chosen: none by default, its summary under it;
        // the pane is started with it, and is sent it.
        if phone {
            page.wait(
                "New session, and its sheet",
                "!!document.querySelector('.sheet .tile-pick') || (document.querySelector('.new-session')?.click(), false)",
            );
            page.js("[...document.querySelectorAll('.sheet .tile-pick')].find(b => b.innerText.includes('Claude Code')).click()");
        } else {
            page.wait(
                "New pane, and its dialog",
                "!!document.querySelector('.dialog .role-pick') || ([...document.querySelectorAll('.side-actions button')].find(b => b.innerText.includes('Pane'))?.click(), false)",
            );
        }
        page.wait(
            "the role choice, none chosen, with the owner's own role in it",
            "(p => p && p.offsetParent && p.value === '' && [...p.options].some(o => o.value === 'copy-editor') && p.closest('.role-pick-row').innerText.includes('No role'))(document.querySelector('.dialog .role-pick'))",
        );
        set(&page, ".dialog .role-pick", "copy-editor");
        page.wait(
            "the role's summary under it",
            "document.querySelector('.dialog .role-pick-says').innerText.startsWith('Reads every word a user will see.')",
        );
        page.js(
            "document.querySelector('.dialog .role-pick-row').scrollIntoView({block: 'center'})",
        );
        // The sheet's other boxes are as they were before roles; the role's
        // own is a full target, and nothing is wider than the page.
        if phone {
            assert_eq!(
                page.js("document.querySelector('.dialog .role-pick').getBoundingClientRect().height >= 43.5 && [...document.querySelectorAll('.dialog *')].every(x => !x.offsetParent || x.getBoundingClientRect().right <= innerWidth + 0.5) && document.documentElement.scrollWidth <= innerWidth"),
                json!(true),
                "{}",
                page.js(MISFITS)
            );
        }
        shot(&page, &format!("{tag}-7-new-pane"));
        page.js("[...document.querySelectorAll('.dialog')].pop().querySelector('button[type=submit]').click()");
        let pane = wait_for("the pane with its role", 20, || {
            s.call("pane.list", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["role"] == json!("copy-editor"))
                .cloned()
        });
        page.wait(
            "it says the role is being sent",
            "[...document.querySelectorAll('.toast')].some(t => t.innerText.includes('It is being sent its role: Copy editor'))",
        );
        let name = pane["name"].as_str().unwrap();
        let body = wait_for("the role, sent", 20, || sent_to(&s, name).pop());
        assert!(
            body.starts_with(&format!("{HEADER}\n\n# Role: critic")),
            "{body}"
        );
        assert!(
            body.contains("You are working alone in this project."),
            "{body}"
        );
        // And the pane's record says its role.
        page.wait(
            "the pane says its role",
            "[...document.querySelectorAll('.chat-title, .chip')].some(e => e.innerText.includes('copy-editor'))",
        );
        shot(&page, &format!("{tag}-8-pane"));

        // Its menu sends the role again, and says so.
        if !phone {
            let id = pane["id"].as_str().unwrap();
            page.right_click(&format!(
                "document.querySelector('.side-pane[data-id=\"{id}\"]')"
            ));
            page.choose("Send its role again");
            page.wait(
                "it says what it sent",
                &format!(
                    "[...document.querySelectorAll('.toast')].some(t => t.innerText.includes({}))",
                    json!(format!(
                        "Sent {name} its role (copy-editor, the owner's text)"
                    ))
                ),
            );
            assert_eq!(sent_to(&s, name).len(), 2);
            // A pane with no role has no such item.
            page.right_click(&format!(
                "document.querySelector('.side-pane[data-id=\"{other}\"]')"
            ));
            assert_eq!(
                page.js("[...document.querySelectorAll('.menu-item')].some(i => i.innerText.startsWith('Send its role again'))"),
                json!(false)
            );
        }

        if std::env::var_os("SPANDECK_SHOTS").is_some() && size == Size::Phone {
            s.call(
                "look.set",
                json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
            );
            page.wait(
                "the light theme",
                "getComputedStyle(document.documentElement).colorScheme === 'light' || document.documentElement.hasAttribute('data-themed')",
            );
            page.goto("#/fleet");
            open_roles(&page, true);
            shot(&page, "phone-light-1-list");
            click(&page, "the critic", ".ro-row[data-role=critic]");
            page.wait(
                "the prompt",
                "!!document.querySelector('.ro-role-view .ro-prompt')",
            );
            shot(&page, "phone-light-2-role");
            click(&page, "Edit", ".ro-edit");
            page.wait("the editor", "!!document.querySelector('.ro-f-prompt')");
            shot(&page, "phone-light-3-editor");
        }
    }

    #[test]
    #[ignore = "merge tier: a browser at a phone's size"]
    fn roles_are_viewed_changed_and_chosen_on_a_phone() {
        the_owners_request(Size::Phone);
    }

    #[test]
    #[ignore = "merge tier: a browser at the narrowest phone's size"]
    fn roles_are_viewed_changed_and_chosen_at_320_px() {
        the_owners_request(Size::Small);
    }

    #[test]
    #[ignore = "merge tier: a browser at a desktop's size"]
    fn roles_are_viewed_changed_and_chosen_on_a_desktop() {
        the_owners_request(Size::Desktop);
    }
}
