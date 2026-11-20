//! A station is renamed, and no two stations have one name (DECISIONS, the
//! owner's request of 20 Nov 2026).
//!
//! What happened: spandeck was put inside a WSL distro on a Windows machine
//! that was already the station `rtx6000`. The distro called itself
//! `RTX6000`, joined under that name, and the fleet listed two stations
//! whose names differ only by capitals. Renamed by hand in its config file,
//! it was then listed twice, once under the old name as a station that never
//! answers, and removing that entry would have told the live station to
//! leave. Each test here is one part of that.
mod common;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

/// `wait_for`, saying what `context` returns when time runs out.
fn wait_or<T>(
    what: &str,
    secs: u64,
    f: impl FnMut() -> Option<T>,
    context: impl Fn() -> String,
) -> T {
    wait_for_or(what, secs, context, f)
}

/// The stations a station lists, as (name, state), after looking again.
fn listed(s: &Station) -> Vec<(String, String)> {
    s.call("fleet.refresh", json!({}))["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["station"].as_str().unwrap().to_string(),
                x["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Every station of the fleet lists exactly these, all online.
fn wait_fleet(stations: &[&Station], names: &[&str], what: &str) {
    let mut want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    want.sort();
    for s in stations {
        wait_or(
            &format!("{what}: {} to list {want:?}", s.name),
            40,
            || {
                // A machine that has not joined is not of the fleet.
                let last: Vec<(String, String)> = listed(s)
                    .into_iter()
                    .filter(|(_, state)| state != "not_linked")
                    .collect();
                let mut names: Vec<String> = last.iter().map(|(n, _)| n.clone()).collect();
                names.sort();
                (names == want && last.iter().all(|(_, state)| state == "online")).then_some(())
            },
            || format!("it lists {:?}", listed(s)),
        );
    }
}

fn name_of(s: &Station) -> String {
    s.call("station.info", json!({}))["station"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The station's config file with another name in it: a rename by hand.
fn rename_in_config(s: &Station, from: &str, to: &str) {
    let file = s.root.join("config/config.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    let line = format!("station = \"{from}\"");
    assert!(text.contains(&line), "{text}");
    std::fs::write(&file, text.replace(&line, &format!("station = \"{to}\""))).unwrap();
}

fn restart(s: &mut Station) {
    s.kill9();
    s.launch();
}

fn recent(s: &Station) -> Vec<Value> {
    s.call("msg.recent", json!({}))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The owner's request: "we should add the ability to rename a station".
/// A station of a fleet of three is renamed from another station's deck
/// call while it has a pane, a place in a global layout, a look and a pin,
/// mail, and (at a station that is off during the rename) a message and a
/// stop waiting for it. Nothing that named it breaks, on any station; the
/// station that was off hears of it when it is back; it is renamed back
/// from its own command line; and its old name can be "removed" without
/// touching it.
#[test]
#[ignore = "merge tier: three stations, each killed or restarted, about a minute"]
fn a_station_is_renamed_and_nothing_that_named_it_breaks() {
    let (a, mut b, mut c) = Station::three();

    // Charlie is away: bravo is given a message and a stop for it, which
    // wait. Then bravo is off, and stays off through the rename.
    c.kill9();
    wait_for("bravo to see charlie away", 30, || {
        (b.sees("charlie").as_deref() == Some("unreachable")).then_some(())
    });
    b.call(
        "msg.send",
        json!({"to": "you@charlie", "body": "sent while charlie was away", "kind": "note"}),
    );
    let stop = b.call("stop.all", json!({"scope": "charlie"}));
    assert_eq!(stop["stops"][0]["status"], json!("unreachable"), "{stop}");
    b.kill9();
    c.launch();
    wait_for("alpha to see charlie again", 30, || {
        (a.sees("charlie").as_deref() == Some("online")).then_some(())
    });

    // What names charlie: a pane, a global layout's leaf, a pin, a look,
    // and mail to one of its panes.
    let sh = c.call("pane.spawn", json!({"agent": "shell", "name": "sh"}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.call(
        "pane.send_text",
        json!({"pane": sh, "text": "echo before-$((6*7))"}),
    );
    c.wait_text(&sh, "before-42");
    let pid_before = c.pane(&sh)["pid"].clone();
    let reader = c.fake(
        json!({"steps": [{"turn_end": "ready"}]}),
        json!({"name": "reader"}),
    );
    c.wait_state(&reader, "done");
    a.call(
        "deck.layout.save",
        json!({"layout": {"name": "all", "tree": {"id": sh, "station": "charlie"},
            "hidden": []}}),
    );
    a.call(
        "deck.pin",
        json!({"pin": {"kind": "pane", "station": "charlie", "id": sh}}),
    );
    a.call(
        "look.set",
        json!({"level": "pane", "key": format!("charlie/{sh}"), "choice": {"set": {"font.size": 15}}}),
    );
    a.call(
        "msg.send",
        json!({"to": "reader@charlie", "body": "sent before the rename", "kind": "note"}),
    );
    wait_for(
        "charlie to hold the layout, the look and the mail",
        20,
        || {
            let deck = c.call("deck.get", json!({}));
            let look = c.call("look.get", json!({}));
            (deck["layouts"][0]["tree"]["station"] == json!("charlie")
                && look["panes"][format!("charlie/{sh}")].is_object()
                && recent(&c)
                    .iter()
                    .any(|m| m["body"] == json!("sent before the rename")))
            .then_some(())
        },
    );

    // ---- The rename, asked of charlie through alpha, as alpha's deck does.
    let out = a.call(
        "peer.call",
        json!({"station": "charlie", "method": "station.rename", "params": {"name": "delta"}}),
    );
    assert_eq!(out["renamed"], json!(true), "{out}");
    let says = out["says"].as_str().unwrap();
    assert!(says.contains("charlie is now delta"), "{says}");
    // Every other station said, when last seen, that it knows of renames.
    assert!(!says.contains("from before"), "{says}");

    // Every station that is on lists the new name, and no station twice.
    wait_for("charlie to answer as delta", 30, || {
        c.try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["station"] == json!("delta"))
            .map(|_| ())
    });
    let on = |s: &Station, want: &[(&str, &str)]| {
        wait_or(
            &format!("{} to list {want:?}", s.name),
            40,
            || {
                let mut got = listed(s);
                got.sort();
                let mut want: Vec<(String, String)> = want
                    .iter()
                    .map(|(n, st)| (n.to_string(), st.to_string()))
                    .collect();
                want.sort();
                (got == want).then_some(())
            },
            || format!("it lists {:?}", listed(s)),
        );
    };
    on(
        &a,
        &[
            ("alpha", "online"),
            ("bravo", "unreachable"),
            ("delta", "online"),
        ],
    );
    on(
        &c,
        &[
            ("alpha", "online"),
            ("bravo", "unreachable"),
            ("delta", "online"),
        ],
    );
    let view = a.call("fleet.view", json!({}));
    assert_eq!(view["former"]["charlie"], json!("delta"), "{view}");

    // The pane was not restarted, and is typed into.
    let pane = c.pane(&sh);
    assert_eq!(pane["pid"], pid_before, "the pane's program was kept");
    assert_eq!(pane["exited"], json!(false), "{pane}");
    assert!(c.text(&sh).contains("before-42"));
    c.call(
        "pane.send_text",
        json!({"pane": sh, "text": "echo after-$((6*8))"}),
    );
    c.wait_text(&sh, "after-48");
    // Through alpha too, by the new name and by the old one.
    for name in ["delta", "charlie"] {
        let text = a.call(
            "peer.call",
            json!({"station": name, "method": "pane.text", "params": {"pane": sh, "history": 50}}),
        );
        assert!(text.to_string().contains("after-48"), "{name}: {text}");
        let got = a.call(
            "peer.call",
            json!({"station": name, "method": "pane.get", "params": {"pane": format!("{sh}@{name}")}}),
        );
        assert_eq!(got["id"], json!(sh), "{name}: {got}");
    }

    // The layout, the pin and the look name delta, on both stations.
    for s in [&a, &c] {
        wait_or(
            &format!("{}'s layout, pin and look to name delta", s.name),
            30,
            || {
                let deck = s.call("deck.get", json!({}));
                let look = s.call("look.get", json!({}));
                (deck["layouts"][0]["tree"] == json!({"id": sh, "station": "delta"})
                    && deck["pins"] == json!([{"kind": "pane", "station": "delta", "id": sh}])
                    && look["panes"][format!("delta/{sh}")]["set"]["font.size"] == json!(15)
                    && look["panes"].get(format!("charlie/{sh}")).is_none())
                .then_some(())
            },
            || {
                format!(
                    "{} / {}",
                    s.call("deck.get", json!({})),
                    s.call("look.get", json!({}))["panes"]
                )
            },
        );
    }
    // And they are written so, not only read so: a station from before
    // renames takes the document as it is written.
    let written = || std::fs::read_to_string(a.root.join("config/deck.json")).unwrap_or_default();
    wait_or(
        "the deck document to be written with the new name",
        30,
        || (written().contains("\"delta\"") && !written().contains("\"charlie\"")).then_some(()),
        written,
    );

    // History from before is found under the new name, at both ends.
    for s in [&a, &c] {
        let mail = recent(s);
        let before = mail
            .iter()
            .find(|m| m["body"] == json!("sent before the rename"))
            .unwrap_or_else(|| panic!("{}: {mail:?}", s.name));
        assert_eq!(before["to"], json!("reader@delta"), "{}", s.name);
    }
    let inbox = c.call_as_pane(&reader, "msg.inbox", json!({"all": true, "peek": true}));
    assert!(
        inbox
            .as_ref()
            .is_ok_and(|i| i.to_string().contains("sent before the rename")),
        "the pane's own mail is still its mail: {inbox:?}"
    );
    // An address with the old name still reaches the pane.
    a.call(
        "msg.send",
        json!({"to": "reader@charlie", "body": "sent to the old name", "kind": "note"}),
    );
    wait_for("mail sent to the old name to arrive", 20, || {
        recent(&c)
            .iter()
            .any(|m| m["body"] == json!("sent to the old name") && m["to"] == json!("reader@delta"))
            .then_some(())
    });

    // The ship's log and the notice, which every deck shows.
    let log = c.log("station.renamed");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["data"], json!({"from": "charlie", "to": "delta"}));
    assert!(
        log[0]["actor"].as_str().unwrap().contains("via alpha"),
        "{log:?}"
    );
    assert_eq!(a.log("fleet.renamed").len(), 1);
    wait_for("the notice", 20, || {
        c.call("notify.list", json!({}))
            .to_string()
            .contains("charlie is now delta")
            .then_some(())
    });

    // ---- Bravo was off through all of it. Back, it hears of the rename
    // from whichever station it sees: no station twice, and what waited
    // for charlie is delivered to delta.
    b.launch();
    wait_fleet(
        &[&a, &b, &c],
        &["alpha", "bravo", "delta"],
        "after bravo is back",
    );
    wait_or(
        "the stop that waited for charlie to be delivered to delta",
        30,
        || {
            b.call("stop.list", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["station"] == json!("delta") && s["status"] == json!("confirmed"))
                .then_some(())
        },
        || b.call("stop.list", json!({})).to_string(),
    );
    wait_or(
        "the message that waited for charlie to arrive at delta",
        30,
        || {
            recent(&c)
                .iter()
                .any(|m| {
                    m["body"] == json!("sent while charlie was away")
                        && m["to"] == json!("you@delta")
                })
                .then_some(())
        },
        || format!("{:?}", recent(&c)),
    );
    assert_eq!(b.log("fleet.renamed").len(), 1, "moved once at bravo");
    let deck = b.call("deck.get", json!({}));
    assert_eq!(
        deck["layouts"][0]["tree"]["station"],
        json!("delta"),
        "{deck}"
    );

    // A name another station has, or had, is refused in a sentence.
    for (name, why) in [
        ("Alpha", "differs from Alpha only by capitals"),
        ("bravo", "already called bravo"),
        ("not a name", "letters, digits"),
    ] {
        let no = c
            .try_call("station.rename", json!({"name": name}))
            .unwrap_err();
        assert!(no.message.contains(why), "{name}: {}", no.message);
    }
    let no = a
        .try_call("station.rename", json!({"name": "Charlie"}))
        .unwrap_err();
    assert!(no.message.contains("delta's name"), "{}", no.message);
    assert_eq!(name_of(&c), "delta");

    // ---- Renamed back, from its own command line.
    let out = c
        .command(SPANDECK)
        .args(["station", "rename", "charlie"])
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && said.contains("delta is now charlie"),
        "{said}"
    );
    wait_fleet(
        &[&a, &b, &c],
        &["alpha", "bravo", "charlie"],
        "renamed back",
    );
    for s in [&a, &b, &c] {
        wait_for("the layout to name charlie again", 30, || {
            let deck = s.call("deck.get", json!({}));
            (deck["layouts"][0]["tree"]["station"] == json!("charlie")).then_some(())
        });
    }
    assert_eq!(c.pane(&sh)["pid"], pid_before, "still the same program");
    // Its other name still finds it.
    let got = a.call(
        "peer.call",
        json!({"station": "delta", "method": "station.info", "params": {}}),
    );
    assert_eq!(got["station"], json!("charlie"));

    // ---- "Remove" of a name it had removes nothing.
    let fleet = a.call("trust.status", json!({}))["fleet"].clone();
    let out = a.call("fleet.remove", json!({"station": "delta"}));
    assert_eq!(out["removed"], json!(false), "{out}");
    assert_eq!(out["former_name_of"], json!("charlie"));
    assert!(
        out["says"].as_str().unwrap().contains("was not touched"),
        "{out}"
    );
    assert_eq!(
        a.call("trust.status", json!({}))["fleet"],
        fleet,
        "the key stayed"
    );
    assert_eq!(c.pane(&sh)["exited"], json!(false));
    wait_fleet(
        &[&a, &b, &c],
        &["alpha", "bravo", "charlie"],
        "after the remove",
    );
}

/// The incident itself: a fleet lists a station under a name it no longer
/// has, because it was renamed by hand in its config file on a spandeck
/// from before renames (`rename = false` makes a station behave as one).
/// "Remove" of the old entry must not reach the live station: its address
/// is the live station's. Once that station runs a spandeck that knows of
/// renames, the others find it by its address and the old entry goes, with
/// its name left as a former name. And a rename by hand on such a spandeck
/// leaves no old entry at all.
#[test]
#[ignore = "merge tier: three stations, one restarted three times"]
fn a_station_renamed_by_hand_is_not_listed_twice_and_removing_its_old_name_touches_nothing() {
    let old = "[flags]\nrename = false\n";
    let ports = [free_port(), free_port(), free_port()];
    let peers = |i: usize| -> Vec<u16> { (0..3).filter(|j| *j != i).map(|j| ports[j]).collect() };
    let a = Station::in_fleet("alpha", ports[0], &peers(0), "");
    let b = Station::in_fleet("bravo", ports[1], &peers(1), "");
    let mut c = Station::in_fleet("charlie", ports[2], &peers(2), old);
    wait_fleet(&[&a, &b, &c], &["alpha", "bravo", "charlie"], "at first");

    // ---- Mixed versions: a rename while a member is on a spandeck from
    // before renames is allowed, and says which station will go on
    // showing the old name. That station does: it lists both.
    let out = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "station.rename", "params": {"name": "beta"}}),
    );
    let says = out["says"].as_str().unwrap();
    assert!(
        says.contains("charlie runs a spandeck from before stations could be renamed")
            && says.contains("will list bravo as well"),
        "{says}"
    );
    wait_fleet(&[&a, &b], &["alpha", "beta", "charlie"], "bravo renamed");
    wait_or(
        "the station from before to list both names",
        30,
        || {
            let l = listed(&c);
            (l.iter().any(|(n, _)| n == "bravo") && l.contains(&("beta".into(), "online".into())))
                .then_some(())
        },
        || format!("{:?}", listed(&c)),
    );
    // What it says of the fleet does not bring the old name back.
    std::thread::sleep(Duration::from_secs(1));
    assert!(!listed(&a).iter().any(|(n, _)| n == "bravo"));
    let sh = c.call("pane.spawn", json!({"agent": "shell", "name": "sh"}))["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Renamed by hand, as on 0.4.19: the others list both names.
    rename_in_config(&c, "charlie", "delta");
    restart(&mut c);
    assert_eq!(name_of(&c), "delta");
    for s in [&a, &b] {
        wait_or(
            "the old entry beside the new one, as before renames",
            30,
            || {
                let l = listed(s);
                (l.iter().any(|(n, _)| n == "charlie")
                    && l.contains(&("delta".into(), "online".into())))
                .then_some(())
            },
            || format!("{:?}", listed(s)),
        );
    }
    let sh2 = c.call("pane.spawn", json!({"agent": "shell", "name": "sh2"}))["id"]
        .as_str()
        .unwrap()
        .to_string();

    // ---- Removing the old entry: nothing is closed, no key is replaced,
    // the live station stays in the fleet. Only the name goes, here.
    let fleet = a.call("trust.status", json!({}))["fleet"].clone();
    let out = a.call("fleet.remove", json!({"station": "charlie"}));
    assert_eq!(out["removed"], json!(false), "{out}");
    assert_eq!(out["former_name_of"], json!("delta"), "{out}");
    let says = out["says"].as_str().unwrap();
    assert!(
        says.contains("former name of delta") && says.contains("no key was replaced"),
        "{says}"
    );
    std::thread::sleep(Duration::from_secs(2));
    for s in [&a, &b, &c] {
        assert_eq!(
            s.call("trust.status", json!({}))["fleet"],
            fleet,
            "{}: the fleet's key was not replaced",
            s.name
        );
    }
    assert_eq!(
        c.pane(&sh2)["exited"],
        json!(false),
        "its panes were not closed"
    );
    assert!(c.log("station.left").is_empty(), "it was not told to leave");
    assert_eq!(a.sees("delta").as_deref(), Some("online"));
    assert!(a.log("fleet.removed").is_empty());
    let _ = sh;

    // A station told to leave under a name that is not its own refuses:
    // what a station from before this change would have sent it.
    let no = a
        .try_call(
            "peer.call",
            json!({"station": "delta", "method": "station.leave", "params": {"station": "charlie"}}),
        )
        .unwrap_err();
    assert!(
        no.message.contains("this station is delta, not charlie"),
        "{}",
        no.message
    );

    // ---- That station is updated (it now knows of renames, though not
    // what it was called). The others find it by its address: the old
    // entry goes on every station, and the old name finds delta.
    let file = c.root.join("config/config.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace(old, "")).unwrap();
    restart(&mut c);
    wait_fleet(
        &[&a, &b, &c],
        &["alpha", "beta", "delta"],
        "once delta knows of renames",
    );
    for s in [&a, &b] {
        let view = s.call("fleet.view", json!({}));
        assert_eq!(
            view["former"]["charlie"],
            json!("delta"),
            "{}: {view}",
            s.name
        );
        let got = s.call(
            "peer.call",
            json!({"station": "charlie", "method": "station.info", "params": {}}),
        );
        assert_eq!(got["station"], json!("delta"));
    }
    // It is told its own former name by the others, and says so once.
    wait_for("delta to learn what it was called", 30, || {
        c.call("notify.list", json!({}))
            .to_string()
            .contains("charlie is now delta")
            .then_some(())
    });

    // ---- By hand again, on a spandeck that knows of renames: the station
    // itself sees that its name changed, and no station lists it twice.
    rename_in_config(&c, "delta", "echo");
    restart(&mut c);
    wait_fleet(&[&a, &b, &c], &["alpha", "beta", "echo"], "renamed by hand");
    let log = c.log("station.renamed");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["actor"], json!("its config file"));
    for old_name in ["charlie", "delta"] {
        let got = b.call(
            "peer.call",
            json!({"station": old_name, "method": "station.info", "params": {}}),
        );
        assert_eq!(got["station"], json!("echo"), "{old_name}");
    }
}

/// "Now there are 2 RTX6000 listed in stations": a station whose name is a
/// member's, capitals aside, is not let in under it. The request says so;
/// the owner approves it under another name, which the station takes as it
/// joins; or it names itself as it asks.
#[test]
#[ignore = "merge tier: four stations, two of them join"]
fn a_station_whose_name_is_taken_joins_under_another() {
    let key = "5a".repeat(32);
    let fleet = Station::several(&[
        ("rtx6000", Some(&key)),
        ("spark2", Some(&key)),
        ("RTX6000", None),
        ("Spark2", None),
    ]);
    let (a, b, wsl, other) = (&fleet[0], &fleet[1], &fleet[2], &fleet[3]);
    wait_for("the fleet of two", 30, || {
        (a.sees("spark2").as_deref() == Some("online")
            && b.sees("rtx6000").as_deref() == Some("online"))
        .then_some(())
    });

    // ---- It asks under its own name. The request is listed on both
    // stations with the fact, and a name to give it.
    wait_for("the newcomer to see a station to ask", 30, || {
        wsl.sees("rtx6000").map(|_| ())
    });
    let asked = wsl.call("fleet.join", json!({"station": "rtx6000"}));
    let code = asked["code"].as_str().unwrap().to_string();
    for s in [a, b] {
        let req = wait_for("the request", 20, || {
            s.call("trust.status", json!({}))["requests"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["code"] == json!(code))
                .cloned()
        });
        assert_eq!(
            req["clash"],
            json!({"with": "rtx6000", "exact": false}),
            "{}: {req} {:?}",
            s.name,
            listed(s)
        );
        assert_eq!(req["suggest"], json!("RTX6000-2"), "{req}");
        assert_eq!(req["can_rename"], json!(true));
    }
    let notice = a.call("notify.list", json!({})).to_string();
    assert!(
        notice.contains(
            "RTX6000 has the same name as rtx6000, a station of this fleet, capitals aside"
        ),
        "{notice}"
    );
    // Approved as it is: refused, in a sentence, and it goes on waiting.
    let no = b
        .try_call("trust.approve", json!({"request": code}))
        .unwrap_err();
    assert!(
        no.message.contains("capitals aside")
            && no
                .message
                .contains(&format!("spandeck approve {code} --as RTX6000-2")),
        "{}",
        no.message
    );
    // Nor under a name that is another member's.
    let no = a
        .try_call("trust.approve", json!({"request": code, "as": "SPARK2"}))
        .unwrap_err();
    assert!(no.message.contains("only by capitals"), "{}", no.message);
    // Approved under another name, at the station that was not asked,
    // from the command line.
    let out = b
        .command(SPANDECK)
        .args(["approve", &code, "--as", "rtx6000-wsl"])
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && said.contains("approved station RTX6000 as rtx6000-wsl"),
        "{said}"
    );
    wait_for("it to take the name it was given", 40, || {
        wsl.try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["station"] == json!("rtx6000-wsl"))
            .map(|_| ())
    });
    wait_fleet(
        &[a, b, wsl],
        &["rtx6000", "spark2", "rtx6000-wsl"],
        "after the join",
    );
    // Never listed under the name it asked with.
    assert!(a.call("fleet.view", json!({}))["twins"]
        .as_array()
        .unwrap()
        .is_empty());

    // ---- The other one names itself as it asks.
    let no = other
        .try_call(
            "fleet.join",
            json!({"station": "spark2", "as": "has space"}),
        )
        .unwrap_err();
    assert!(no.message.contains("letters, digits"), "{}", no.message);
    wait_for("the second newcomer to see a station to ask", 30, || {
        other.sees("spark2").map(|_| ())
    });
    let out = other
        .command(SPANDECK)
        .args(["fleet", "--join", "spark2", "--as", "spark3"])
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let req = wait_for("its request", 20, || {
        a.call("trust.status", json!({}))["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == json!("spark3"))
            .cloned()
    });
    assert!(req.get("clash").is_none(), "{req}");
    a.call("trust.approve", json!({"request": req["id"]}));
    wait_for("it to take the name it asked under", 40, || {
        other
            .try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["station"] == json!("spark3"))
            .map(|_| ())
    });
    wait_fleet(
        &[a, b, wsl, other],
        &["rtx6000", "spark2", "rtx6000-wsl", "spark3"],
        "after both joined",
    );
}

/// The owner's fleet as it was: two members whose names differ only by
/// capitals. It keeps working as it did, each station is found by its own
/// spelling and only by it, and the fleet says the two are alike so that
/// one can be renamed; which then ends it.
#[test]
#[ignore = "merge tier: three stations, one renamed"]
fn two_members_whose_names_differ_only_by_capitals_are_each_found_by_their_own() {
    let key = "5a".repeat(32);
    let fleet = Station::several(&[
        ("rtx6000", Some(&key)),
        ("RTX6000", Some(&key)),
        ("spark2", Some(&key)),
    ]);
    let (low, up, s) = (&fleet[0], &fleet[1], &fleet[2]);
    wait_fleet(&[low, up, s], &["rtx6000", "RTX6000", "spark2"], "the pair");
    assert_eq!(
        s.call("fleet.view", json!({}))["twins"],
        json!([["RTX6000", "rtx6000"]])
    );
    // Each by its own name, exactly.
    for name in ["rtx6000", "RTX6000"] {
        let got = s.call(
            "peer.call",
            json!({"station": name, "method": "station.info", "params": {}}),
        );
        assert_eq!(got["station"], json!(name));
    }
    // `--station` picks by the name as written: only that one restarts.
    let started = |x: &Station| x.call("station.info", json!({}))["started_ms"].clone();
    let (low_before, up_before) = (started(low), started(up));
    let out = s
        .command(SPANDECK)
        .args(["restart", "--station", "RTX6000"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    wait_for("RTX6000 to restart", 30, || {
        up.try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["started_ms"] != up_before)
            .map(|_| ())
    });
    assert_eq!(started(low), low_before, "rtx6000 was not restarted");
    // A spelling that is neither could be either: it is neither.
    let no = s
        .try_call(
            "peer.call",
            json!({"station": "Rtx6000", "method": "station.info", "params": {}}),
        )
        .unwrap_err();
    assert!(no.message.contains("not in the fleet"), "{}", no.message);
    // Mail goes to the one named.
    for (station, body) in [("rtx6000", "for the lower"), ("RTX6000", "for the upper")] {
        s.call(
            "msg.send",
            json!({"to": format!("you@{station}"), "body": body, "kind": "note"}),
        );
    }
    for (x, body, not) in [
        (low, "for the lower", "for the upper"),
        (up, "for the upper", "for the lower"),
    ] {
        wait_for("each to get its own mail", 20, || {
            recent(x)
                .iter()
                .any(|m| m["body"] == json!(body))
                .then_some(())
        });
        assert!(!recent(x).iter().any(|m| m["body"] == json!(not)));
    }

    // Renaming one ends it, on every station.
    let out = s.call(
        "peer.call",
        json!({"station": "RTX6000", "method": "station.rename", "params": {"name": "rtx6000-wsl"}}),
    );
    assert_eq!(out["renamed"], json!(true), "{out}");
    wait_fleet(
        &[low, up, s],
        &["rtx6000", "rtx6000-wsl", "spark2"],
        "the pair no more",
    );
    for x in [low, up, s] {
        assert_eq!(x.call("fleet.view", json!({}))["twins"], json!([]));
    }
    // The old spelling finds the renamed one; the other is still itself.
    let got = s.call(
        "peer.call",
        json!({"station": "RTX6000", "method": "station.info", "params": {}}),
    );
    assert_eq!(got["station"], json!("rtx6000-wsl"));
    let got = s.call(
        "peer.call",
        json!({"station": "rtx6000", "method": "station.info", "params": {}}),
    );
    assert_eq!(got["station"], json!("rtx6000"));
}

#[cfg(unix)]
fn executable(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    spandeck_terminal::sys::set_executable(path).unwrap();
}

/// The deck's part, in a real browser. A fleet whose two stations differ
/// only by capitals shows one quiet line offering to rename one; "Rename…"
/// is in a station's menu, refuses a taken name in the station's own
/// sentence, and renames. A newcomer whose name is taken has a card that
/// says so, with a box for its name, filled with a free one. And Add a
/// machine, when Tailscale SSH holds the connection until the owner
/// confirms it (the same day's fault: twelve seconds, then "probably
/// asleep or off"), says that, shows Tailscale's address as the only link,
/// offers a name for the new station, and says what to do when nobody
/// confirmed.
#[test]
#[cfg(unix)]
#[ignore = "merge tier: a browser, three stations and a stand-in ssh"]
fn the_deck_renames_a_station_names_a_newcomer_and_waits_for_tailscale() {
    use common::browser::Page;
    let dir = tempfile::tempdir().unwrap();
    let at = |name: &str| dir.path().join(name);
    std::fs::create_dir_all(at("standin")).unwrap();
    // A machine under the owner's login that calls itself SPARK9 and that
    // Tailscale calls spark9-wsl.
    std::fs::write(
        at("standin/status.json"),
        json!({"Self": {"UserID": 7, "HostName": "rtx6000", "TailscaleIPs": ["100.64.0.1"]},
            "User": {"7": {"LoginName": "owner@example.com"}},
            "Peer": {"key-wsl": {"ID": "n-wsl", "HostName": "SPARK9", "DNSName": "spark9-wsl.tail0000.ts.net.",
                "OS": "linux", "UserID": 7, "Online": true, "TailscaleIPs": ["100.64.0.21"]}}})
        .to_string(),
    )
    .unwrap();
    executable(
        &at("standin/tailscale"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in version) echo 1.0 ;; status) cat {} ;; *) exit 1 ;; esac\n",
            at("standin/status.json").display()
        ),
    );
    // "ssh" as Tailscale SSH answers when it wants the owner to sign in
    // again: it prints the address and holds the connection. Nobody
    // confirms (no `go` file): the connection ends as ssh's does.
    executable(
        &at("standin/ssh"),
        &format!(
            r##"#!/bin/sh
echo "# Tailscale SSH requires an additional check." >&2
echo "# To authenticate, visit: https://login.tailscale.com/a/1a2b3c4d5e6f" >&2
echo "motd: <b>see</b> https://evil.example/login.tailscale.com/a/zz" >&2
n=0
while [ ! -e {go} ] && [ $n -lt 12 ]; do sleep 0.25; n=$((n+1)); done
if [ ! -e {go} ]; then
  echo "ssh: connect to host $2 port 22: Connection timed out" >&2
  exit 255
fi
IFS= read -r boot
echo "@spandeck step install"
echo "@spandeck fail network"
exit 1
"##,
            go = at("go").display()
        ),
    );
    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let key = "5a".repeat(32);
    let ports = [free_port(), free_port(), free_port()];
    let others = |i: usize| -> String {
        (0..3)
            .filter(|j| *j != i)
            .map(|j| format!("\"127.0.0.1:{}\"", ports[j]))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut low = Station::not_started(
        "rtx6000",
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\npeers = [{}]\n",
            at("standin/ssh").display(),
            others(0)
        ),
    );
    // On the port its mates were told of.
    let text = std::fs::read_to_string(low.root.join("config/config.toml")).unwrap();
    std::fs::write(
        low.root.join("config/config.toml"),
        text.replace(
            &format!("web_port = {}", low.port),
            &format!("web_port = {}", ports[0]),
        ),
    )
    .unwrap();
    low.port = ports[0];
    low.env = vec![("PATH".into(), path)];
    std::fs::create_dir_all(low.root.join("state")).unwrap();
    std::fs::write(low.root.join("state/fleet.key"), &key).unwrap();
    low.launch();
    let up = Station::start_keyed(
        "RTX6000",
        ports[1],
        &format!("peers = [{}]\n", others(1)),
        Some(&key),
    );
    let newcomer = Station::start_keyed(
        "Rtx6000",
        ports[2],
        &format!("peers = [{}]\n", others(2)),
        None,
    );
    wait_for("the two members", 30, || {
        (low.sees("RTX6000").as_deref() == Some("online")
            && up.sees("rtx6000").as_deref() == Some("online"))
        .then_some(())
    });

    let page = Page::open(&low, 1300, 900);
    page.goto("#/fleet");
    let dialog_says = |what: &str, needle: &str| {
        page.wait(
            what,
            &format!(
                "(document.querySelector('.dialog')?.innerText || '').includes({}) && document.querySelector('.dialog').innerText",
                json!(needle)
            ),
        )
        .as_str()
        .unwrap()
        .to_string()
    };
    let press = |label: &str| {
        page.wait(
            &format!("the {label} button"),
            &format!(
                "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog button')].find(b => b.innerText.trim() === {}))",
                json!(label)
            ),
        );
    };
    let tiles = "[...document.querySelectorAll('.tiles .tile .tile-head b')].map(b => b.innerText)";

    // ---- One quiet line for the pair that is there already.
    let line = page.wait(
        "the line about the two names",
        "document.querySelector('.twins-line')?.innerText",
    );
    assert!(
        line.as_str()
            .unwrap()
            .contains("RTX6000 and rtx6000 differ only by capitals"),
        "{line}"
    );

    // ---- Rename…, from the station's own menu.
    page.right_click(
        "[...document.querySelectorAll('.tiles .tile')].find(t => t.querySelector('.tile-head b')?.innerText === 'RTX6000')",
    );
    page.choose("Rename…");
    let said = dialog_says("the rename dialog", "New name for RTX6000");
    assert!(
        said.contains("every pane, agent, conversation and terminal stays open"),
        "{said}"
    );
    assert!(
        said.contains("Tailscale name and does not change"),
        "{said}"
    );
    let type_name = |name: &str| {
        page.js(&format!(
            "(i => {{ i.value = {}; i.form.requestSubmit(); return true; }})(document.querySelector('.rename-name'))",
            json!(name)
        ));
    };
    // A taken name: the station's sentence, in the dialog, which stays.
    type_name("rtx6000");
    let why = page.wait(
        "the refusal",
        "document.querySelector('.rename-why')?.innerText",
    );
    assert!(
        why.as_str()
            .unwrap()
            .contains("Another station is already called rtx6000"),
        "{why}"
    );
    assert_eq!(name_of(&up), "RTX6000");
    type_name("rtx6000-wsl");
    page.wait(
        "the dialog to close and the tile to say the new name",
        &format!("!document.querySelector('.dialog') && {tiles}.includes('rtx6000-wsl') && !{tiles}.includes('RTX6000')"),
    );
    page.wait("the line to go", "!document.querySelector('.twins-line')");
    wait_for("the station to have the name", 30, || {
        up.try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["station"] == json!("rtx6000-wsl"))
            .map(|_| ())
    });
    // A link from before the rename opens the station under its new name.
    page.goto("#/s/RTX6000");
    page.wait(
        "the old name's page to be the station's",
        "(document.querySelector('.station-view')?.innerText || '').includes('rtx6000-wsl') && !(document.querySelector('.station-main')?.innerText || '').includes('RTX6000 is unreachable')",
    );
    page.goto("#/fleet");

    // ---- A newcomer whose name is taken: its card says so and has a box
    // for its name, with a free one in it.
    wait_for("the newcomer to see a station to ask", 30, || {
        newcomer.sees("rtx6000").map(|_| ())
    });
    newcomer.call("fleet.join", json!({"station": "rtx6000"}));
    page.wait(
        "the request to be counted",
        "(b => b && b.querySelector('.count') ? (b.click(), true) : false)(document.querySelector('.topbar button[title^=\"Stations and browsers\"]'))",
    );
    let said = dialog_says("the card", "Station Rtx6000");
    assert!(
        said.contains(
            "Rtx6000 has the same name as rtx6000, a station of this fleet, capitals aside."
        ),
        "{said}"
    );
    assert_eq!(
        page.wait(
            "the box",
            "document.querySelector('.dialog .join-name')?.value"
        ),
        json!("Rtx6000-2")
    );
    // With the box emptied it is not let in: the station says why.
    page.js("(i => { i.value = ''; return true; })(document.querySelector('.dialog .join-name'))");
    press("Approve");
    page.wait(
        "the refusal to be said",
        "[...document.querySelectorAll('.toast')].some(t => t.innerText.includes('Approve it under another name'))",
    );
    assert_eq!(name_of(&newcomer), "Rtx6000");
    page.wait(
        "the card again",
        "(i => i ? (i.value = 'rtx-three', true) : false)(document.querySelector('.dialog .join-name'))",
    );
    press("Approve");
    wait_for("the newcomer to join under the name typed", 40, || {
        newcomer
            .try_call("station.info", json!({}))
            .ok()
            .filter(|i| i["station"] == json!("rtx-three"))
            .map(|_| ())
    });
    page.wait(
        "its tile",
        &format!("!document.querySelector('.dialog') && {tiles}.includes('rtx-three')"),
    );

    // ---- Add a machine, with Tailscale SSH waiting for the owner.
    low.call("fleet.add_enable", json!({"on": true}));
    page.wait(
        "the Add a machine tile",
        "(t => t ? (t.click(), true) : false)(document.querySelector('.add-tile'))",
    );
    dialog_says("the machines", "spark9-wsl");
    page.wait(
        "the machine to be picked",
        "(b => b ? (b.click(), true) : false)(document.querySelector('.dialog .add-machine'))",
    );
    // Its station would call itself SPARK9: Tailscale's name for the
    // machine is offered, and said to be that.
    let offered = page.wait(
        "the name offered",
        "document.querySelector('.dialog .add-name')?.value",
    );
    assert_eq!(offered, json!("spark9-wsl"), "{offered}");
    let said = dialog_says("why a name is offered", "The machine calls itself SPARK9");
    assert!(said.contains("Tailscale calls it spark9-wsl"), "{said}");
    page.js("(i => { i.value = 'sam'; return true; })(document.querySelector('.dialog input[aria-label=\"User name on that machine\"]'))");
    press("Connect and install");
    let said = dialog_says(
        "what is waited for",
        "Tailscale is waiting for you to confirm this connection.",
    );
    assert!(said.contains("Nothing is wrong with spark9-wsl"), "{said}");
    // Tailscale's address is a link; nothing else that was printed is.
    let links = page.wait(
        "the link",
        "(a => a.length ? JSON.stringify(a.map(x => x.href)) : false)([...document.querySelectorAll('.dialog a')])",
    );
    assert_eq!(
        links,
        json!("[\"https://login.tailscale.com/a/1a2b3c4d5e6f\"]"),
        "only Tailscale's own address is a link"
    );
    let printed = page.wait(
        "what was printed",
        "(t => t.includes('evil.example') && t)(document.querySelector('.dialog .add-log')?.textContent || '')",
    );
    let printed = printed.as_str().unwrap();
    assert!(
        printed.contains("<b>see</b> https://evil.example/login.tailscale.com/a/zz"),
        "shown as the text it is: {printed}"
    );
    assert_eq!(
        page.js("document.querySelectorAll('.dialog .add-log *').length"),
        json!(0)
    );
    assert_eq!(
        low.call("fleet.add_status", json!({}))["name_as"],
        json!("spark9-wsl")
    );
    // Nobody confirmed: it says so, and what to do. Not "asleep or off".
    let said = dialog_says("the end of the wait", "was not confirmed");
    assert!(
        said.contains("confirm, then Try again: Tailscale remembers it for a while")
            && !said.contains("asleep or off"),
        "{said}"
    );
    let status = low.call("fleet.add_status", json!({}));
    assert_eq!(status["problem"]["kind"], json!("confirm"), "{status}");
    assert_eq!(
        status["confirm"],
        json!("https://login.tailscale.com/a/1a2b3c4d5e6f")
    );

    // ---- Confirmed this time: it goes on by itself, and no longer waits.
    std::fs::write(at("go"), "").unwrap();
    low.call("fleet.add_cancel", json!({}));
    low.call("fleet.add", json!({"machine": "n-wsl", "user": "sam"}));
    let ended = wait_for("the add to go on past the confirmation", 30, || {
        let v = low.call("fleet.add_status", json!({}));
        (v["stage"] == json!("failed")).then_some(v)
    });
    assert_eq!(ended["problem"]["kind"], json!("network"), "{ended}");
    assert_eq!(ended["confirm"], Value::Null, "{ended}");
}
