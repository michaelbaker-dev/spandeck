//! A station reached over the owner's own SSH (DECISIONS.md, 20 Nov 2026).
//!
//! The reason for every test here is the owner's decision of that day: a
//! station with no Tailscale is reached over a command of the owner's own,
//! its deck is opened in the browser of the machine the owner is at, and
//! nothing that works without a link changes. Each test says which rule
//! of that section it holds the build to.
//!
//! Two real stations with folders of their own. The near one has a
//! `[[link]]` whose `cmd` is `env` with the far one's folders, to which a
//! station adds `spandeck link --stdio` as it does to any carrier that is
//! not ssh: the far one's helper runs directly, no SSH is used, and
//! nothing outside the test's own folders is touched. The stand-in agent program; no real agent.
//!
//!   cargo test -p spandeck --test link -- --ignored
#![cfg(unix)]
mod common;

use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

use common::browser::Page;
use common::wire::Deck;
use common::*;
use serde_json::{json, Value};

/// The `[[link]]` that reaches `far` from another station on this
/// machine: a carrier that is told the far station's folders, and finds
/// the `spandeck` this test built first on its PATH.
fn link_to(far: &Station, port: u16) -> String {
    format!(
        "[[link]]\nname = \"devbox\"\nport = {port}\ncmd = [\"env\", \"PATH={}:{}\", \"SPANDECK_STATE_DIR={}\", \"SPANDECK_CONFIG_DIR={}\", \"HOME={}\"]\n",
        std::path::Path::new(SPANDECK).parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default(),
        far.root.join("state").display(),
        far.root.join("config").display(),
        far.root.join("home").display(),
    )
}

struct Linked {
    near: Station,
    far: Station,
    port: u16,
}

fn linked() -> Linked {
    let far = Station::start("far");
    let port = free_port();
    let near = Station::start_with("near", &link_to(&far, port), &[]);
    let l = Linked { near, far, port };
    l.wait_state("up");
    l
}

impl Linked {
    fn row(&self) -> Value {
        self.near.call("link.list", json!({}))[0].clone()
    }

    fn wait_state(&self, state: &str) -> Value {
        wait_for_or(
            &format!("the link to be {state}"),
            30,
            || self.row().to_string(),
            || Some(self.row()).filter(|r| r["state"] == json!(state)),
        )
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// `spandeck link <args>` at the near station, as the owner types it.
    fn cli(&self, args: &[&str]) -> (bool, String) {
        let out = self
            .near
            .command(SPANDECK)
            .arg("link")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// `spandeck link open devbox`: the address it prints, and the id and
    /// the code in it.
    fn open(&self) -> (String, String, String) {
        let (ok, said) = self.cli(&["open", "devbox"]);
        assert!(ok, "{said}");
        let url = said.lines().next().unwrap().trim().to_string();
        let (at, sign) = url.split_once("/#signin=").expect(&said);
        assert_eq!(at, format!("http://localhost:{}", self.port));
        let (id, collect) = sign.split_once('.').unwrap();
        (url.clone(), id.to_string(), collect.to_string())
    }

    /// What the page does with that address: exchange it, through the
    /// link, for the browser's token.
    async fn collect(&self, id: &str, collect: &str) -> Value {
        http()
            .post(self.url("/api/device/collect"))
            .json(&json!({"id": id, "collect": collect}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["result"]
            .clone()
    }

    /// A call to the far station through the link's port, with a token
    /// or without: the status and the answer.
    async fn rpc(&self, token: Option<&str>, method: &str) -> (u16, Value) {
        let mut ask = http()
            .post(self.url("/api/rpc"))
            .json(&json!({"method": method, "params": {}}));
        if let Some(token) = token {
            ask = ask.bearer_auth(token);
        }
        let got = ask.send().await.unwrap();
        (
            got.status().as_u16(),
            got.json::<Value>().await.unwrap_or(Value::Null),
        )
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// The browsers the far station lists as signed in.
fn devices(far: &Station) -> Vec<Value> {
    far.call("trust.status", json!({}))["devices"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The ports a process listens on (TCP, any address).
fn listening(pid: u32) -> BTreeSet<u16> {
    if cfg!(target_os = "linux") {
        // The sockets the process holds, and which of the machine's
        // listening sockets they are.
        let held: BTreeSet<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .flatten()
            .filter_map(|f| std::fs::read_link(f.path()).ok())
            .filter_map(|l| {
                let l = l.to_string_lossy().to_string();
                Some(l.strip_prefix("socket:[")?.strip_suffix(']')?.to_string())
            })
            .collect();
        let mut ports = BTreeSet::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            for line in std::fs::read_to_string(table)
                .unwrap_or_default()
                .lines()
                .skip(1)
            {
                let f: Vec<&str> = line.split_whitespace().collect();
                // State 0A is listening; the tenth field is the inode.
                if f.len() > 9 && f[3] == "0A" && held.contains(f[9]) {
                    let port = f[1].rsplit(':').next().unwrap();
                    ports.insert(u16::from_str_radix(port, 16).unwrap());
                }
            }
        }
        ports
    } else {
        let out = Command::new("lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()])
            .output()
            .expect("lsof, to list what a station listens on");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .filter_map(|l| {
                l.split_whitespace()
                    .rev()
                    .nth(1)?
                    .rsplit(':')
                    .next()?
                    .parse()
                    .ok()
            })
            .collect()
    }
}

/// What a station's API says to a browser that names this machine with
/// another port: the status. (403: "this Host is not served here".)
async fn foreign_host(s: &Station, port: u16) -> u16 {
    http()
        .post(s.url("/api/device/collect"))
        .header("host", format!("localhost:{port}"))
        .json(&json!({"id": "q-none", "collect": "none"}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// A signal by its name, with the system's own `kill`.
fn send(pid: u64, signal: &str) {
    let done = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(done.success(), "kill -{signal} {pid}");
}

/// "Nothing that works today may break ... a station with no link must
/// behave as before (no new listener)". A station without a `[[link]]`
/// listens where it did and nowhere else, answers `link list` with "none",
/// and takes `localhost` with another port for a Host no more than it did.
/// With one, there is exactly one more listening socket: the link's port,
/// on 127.0.0.1.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts three daemons"]
async fn a_station_with_no_link_listens_nowhere_new() {
    let (plain, l) = off(|| (Station::start("plain"), linked())).await;
    let ports = |s: &Station| listening(s.daemon_pid());
    assert_eq!(ports(&plain), BTreeSet::from([plain.port]));
    assert_eq!(ports(&l.far), BTreeSet::from([l.far.port]));
    assert_eq!(ports(&l.near), BTreeSet::from([l.near.port, l.port]));
    assert_eq!(plain.call("link.list", json!({})), json!([]));
    assert!(!plain.root.join("state/links").exists());
    // The link's port is on this machine alone.
    let elsewhere = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("192.0.2.1:9")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .ok();
    if let Some(ip) = elsewhere.filter(|ip| !ip.is_loopback() && !ip.is_unspecified()) {
        assert!(
            std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::new(ip, l.port),
                Duration::from_secs(2)
            )
            .is_err(),
            "the link's port answers at {ip}"
        );
    }
    // A Host of localhost with a port that is no link's is refused by the
    // API of a station that has no helper attached, as it always was.
    assert_eq!(foreign_host(&plain, l.port).await, 403);
    // Every station says it has links, so that a later one can ask.
    for s in [&plain, &l.near, &l.far] {
        let caps = s.call("station.info", json!({}))["caps"].clone();
        assert!(caps.as_array().unwrap().contains(&json!("link")), "{caps}");
    }
}

/// The owner's use: the far station's own deck in the browser of the
/// machine they are at, signed in by `spandeck link open`. Held to:
///  - the page and the API of the far station answer at the local port;
///  - a call with no token is refused, as at any station;
///  - what `link open` prints is not a token, works once, and the entry it
///    makes is listed at the far station and signed out there like any
///    other, after which the browser is locked out;
///  - asked again, the same entry is used, not a second one;
///  - the far station's own token is nowhere at the near station;
///  - the calls of a link are refused to an agent, to a signed-in browser
///    and to the owner's request passed on by another station (rule 7);
///  - the ship's log of both stations says what happened.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons"]
async fn the_far_deck_is_opened_and_signed_in_through_the_link() {
    let l = std::sync::Arc::new(off(linked).await);
    let row = l.row();
    assert_eq!(row["station"], json!("far"), "{row}");
    assert_eq!(row["version"], json!(env!("CARGO_PKG_VERSION")), "{row}");
    assert!(
        row["rtt_ms"].is_u64() || {
            tokio::time::sleep(Duration::from_secs(3)).await;
            l.row()["rtt_ms"].is_u64()
        }
    );

    // The page, at both names a browser may use for this machine.
    for at in ["127.0.0.1", "localhost"] {
        let page = http()
            .get(format!("http://{at}:{}/", l.port))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status().as_u16(), 200, "{at}");
        assert!(page.text().await.unwrap().contains("spandeck"));
    }
    // No token, no call.
    assert_eq!(l.rpc(None, "station.info").await.0, 401);
    assert_eq!(l.rpc(Some("dev.q-made.up"), "station.info").await.0, 401);

    // Signed in over the link.
    assert!(devices(&l.far).is_empty());
    let ll = l.clone();
    let (url, id, code) = off(move || ll.open()).await;
    assert!(!url.contains("token") && !url.contains("dev."), "{url}");
    let got = l.collect(&id, &code).await;
    assert_eq!(got["status"], json!("approved"), "{got}");
    let token = got["token"].as_str().unwrap().to_string();
    assert!(token.starts_with(&format!("dev.{id}.")), "{token}");
    let (status, info) = l.rpc(Some(&token), "station.info").await;
    assert_eq!((status, &info["result"]["station"]), (200, &json!("far")));
    // The address works once.
    assert_eq!(l.collect(&id, &code).await["status"], json!("gone"));

    // An ordinary entry at the far station, named for this machine.
    let listed = devices(&l.far);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["id"], json!(id));
    let name = listed[0]["name"].as_str().unwrap();
    assert!(name.ends_with("(over a link)"), "{name}");

    // Asked again: the same entry, and again an address that works once.
    let ll = l.clone();
    let (_, id2, code2) = off(move || ll.open()).await;
    assert_eq!(id2, id);
    assert_ne!(code2, code);
    assert_eq!(devices(&l.far).len(), 1);
    assert_eq!(l.collect(&id2, &code2).await["token"], json!(token));

    // The calls of a link are for this machine's command line alone.
    let (status, refused) = l.rpc(Some(&token), "link.signin").await;
    assert!(
        status != 200 && refused["error"]["code"] == json!("denied"),
        "{refused}"
    );
    let ll = l.clone();
    off(move || {
        let pane = ll
            .far
            .fake(json!({"steps": [{"work": 600_000}]}), json!({}));
        for method in [
            "link.attach",
            "link.signin",
            "link.list",
            "link.open",
            "link.up",
            "link.down",
        ] {
            let as_agent = ll
                .far
                .call_as_pane(&pane, method, json!({"port": 1, "name": "devbox"}));
            assert_eq!(as_agent.unwrap_err().code, "denied", "{method}");
        }
    })
    .await;
    // Not even with the station's own token, anywhere but its local
    // socket: over HTTP on this machine it is refused.
    for (s, method) in [
        (&l.far, "link.signin"),
        (&l.near, "link.list"),
        (&l.near, "link.open"),
    ] {
        let got = http()
            .post(s.url("/api/rpc"))
            .bearer_auth(s.token())
            .json(&json!({"method": method, "params": {"name": "devbox"}}))
            .send()
            .await
            .unwrap();
        let said = got.json::<Value>().await.unwrap();
        assert_eq!(said["error"]["code"], json!("denied"), "{method}: {said}");
    }

    // The far station's own token never left it.
    let far_token = l.far.token();
    let mut kept = vec![];
    let mut dirs = vec![l.near.root.join("state")];
    while let Some(dir) = dirs.pop() {
        for f in std::fs::read_dir(&dir).unwrap().flatten() {
            match f.file_type().unwrap().is_dir() {
                true => dirs.push(f.path()),
                false => kept.push(f.path()),
            }
        }
    }
    for file in &kept {
        let text = String::from_utf8_lossy(&std::fs::read(file).unwrap_or_default()).to_string();
        assert!(!text.contains(&far_token), "{}", file.display());
        assert!(!text.contains(&token), "{}", file.display());
    }
    // What the near station keeps of the link: a file only this user reads.
    use std::os::unix::fs::PermissionsExt;
    let file = l.near.root.join("state/links/devbox.json");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["device"], json!(id));

    // Signed out at the far station: locked out at once.
    let (ll, gone) = (l.clone(), id.clone());
    off(move || ll.far.call("device.revoke", json!({"device": gone}))).await;
    assert_eq!(l.rpc(Some(&token), "station.info").await.0, 401);
    assert!(devices(&l.far).is_empty());
    // And the next `link open` makes a new entry, not the old one again.
    let ll = l.clone();
    let (_, id3, _) = off(move || ll.open()).await;
    assert_ne!(id3, id);

    // The ship's logs.
    let up = l.near.log("link.up");
    assert_eq!(up.len(), 1, "{up:?}");
    assert_eq!(up[0]["data"]["station"], json!("far"));
    assert_eq!(up[0]["data"]["over"], json!("exec"));
    assert!(up[0]["data"]["carrier"]
        .as_str()
        .unwrap()
        .starts_with("env PATH="));
    assert_eq!(l.near.log("link.signin").len(), 3);
    assert_eq!(l.far.log("link.attached")[0]["data"]["port"], json!(l.port));
    let approved = l.far.log("trust.approved");
    assert_eq!(approved.len(), 2, "{approved:?}");
    assert_eq!(approved[0]["data"]["by"], json!("you (over a link)"));
}

/// "The WebSocket works through it", and a link carries more than one
/// thing at a time: a pane of the far station is attached through the
/// link's port with the browser's own token, what its agent prints is
/// seen, and a key typed there reaches it, while a megabyte is being
/// fetched on another connection of the same link.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons"]
async fn a_far_pane_is_attached_and_typed_into_through_the_link() {
    let l = std::sync::Arc::new(off(linked).await);
    let ll = l.clone();
    let (pane, shell, (_, id, code)) = off(move || {
        let pane = ll.far.fake(
            json!({"steps": [{"wait_file": "go"}, {"say": "printed on the far side"}, {"work": 600_000}]}),
            json!({"name": "worker"}),
        );
        let shell = ll.far.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": "sh", "cwd": ll.far.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        (pane, shell, ll.open())
    })
    .await;
    let token = l.collect(&id, &code).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let deck = Deck::open_with(&token, l.port, json!({"client": "tab-1", "caps": ["busy"]})).await;
    let chan = deck.attach(json!({"pane": pane})).await.unwrap();
    std::fs::write(l.far.root.join("home/go"), "").unwrap();
    deck.shows(chan, "printed on the far side").await;
    // Other connections of the same link, busy meanwhile.
    let busy: Vec<_> = (0..4)
        .map(|_| {
            let (at, token) = (l.url("/api/rpc"), token.clone());
            tokio::spawn(async move {
                for _ in 0..20 {
                    let got = http()
                        .post(&at)
                        .bearer_auth(&token)
                        .json(&json!({"method": "log.query", "params": {}}))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(got.status().as_u16(), 200);
                    got.bytes().await.unwrap();
                }
            })
        })
        .collect();
    let at_shell = deck.attach(json!({"pane": shell})).await.unwrap();
    deck.key(at_shell, "echo typed-$((40+2))\r").await;
    deck.shows(at_shell, "typed-42").await;
    for b in busy {
        b.await.unwrap();
    }
    // A wrong token opens no socket through the link either.
    let url = format!("ws://127.0.0.1:{}/api/ws", l.port);
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    use futures_util::{SinkExt, StreamExt};
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"auth": {"token": "dev.q-made.up", "client": "tab-2"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let first = ws.next().await;
    let refused = match &first {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
            serde_json::from_str::<Value>(t).unwrap()["auth"]["ok"] != json!(true)
        }
        _ => true,
    };
    assert!(refused, "{first:?}");
}

/// "Pings every 2 s, not answering after 6 s ... reconnect ... never a
/// hang", and "hold or drop it without editing config".
///  - The far side stops answering without the carrier ending (it is
///    frozen): within the heartbeat's limit the link says so, the carrier
///    is ended, another is started, and the link is up again by itself.
///  - The carrier is killed: the same, at once.
///  - Held closed by the owner: the browser is answered at once in plain
///    words, the listening port stays, no carrier runs; `up` opens it.
///  - The far station goes on running through all of it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons; waits out a heartbeat"]
async fn a_link_that_stops_answering_says_so_and_comes_back_by_itself() {
    let l = std::sync::Arc::new(off(linked).await);
    let ll = l.clone();
    let pane = off(move || {
        let pane = ll.far.fake(
            json!({"steps": [{"prompt": "build"}, {"work": 600_000}]}),
            json!({}),
        );
        ll.far.wait_state(&pane, "working");
        pane
    })
    .await;
    let first = l.row()["pid"].as_u64().unwrap();

    // Frozen: the carrier runs, and nothing comes back.
    let began = std::time::Instant::now();
    send(first, "STOP");
    let ll = l.clone();
    let (down, back) = off(move || {
        let down = wait_for("the link's end to be logged", 20, || {
            ll.near.log("link.down").into_iter().next()
        });
        (down, ll.wait_state("up"))
    })
    .await;
    let took = began.elapsed();
    assert!(
        down["data"]["why"]
            .as_str()
            .unwrap()
            .contains("no answer to a ping for 6 seconds"),
        "{down}"
    );
    // Six seconds after the last answer, which came up to two seconds
    // before the freeze; then a new carrier and its hello.
    assert!(
        took >= Duration::from_secs(3) && took < Duration::from_secs(12),
        "{took:?}"
    );
    let second = back["pid"].as_u64().unwrap();
    assert_ne!(second, first);
    assert_eq!(l.rpc(None, "station.info").await.0, 401);

    // Killed.
    send(second, "KILL");
    let ll = l.clone();
    let third = off(move || {
        wait_for("another carrier", 20, || {
            let row = ll.row();
            (row["state"] == json!("up") && row["pid"] != json!(second))
                .then(|| row["pid"].as_u64().unwrap())
        })
    })
    .await;
    assert_eq!(l.near.log("link.down").len(), 2);
    assert_eq!(l.rpc(None, "station.info").await.0, 401);

    // Held closed.
    let ll = l.clone();
    let (ok, said) = off(move || ll.cli(&["down", "devbox"])).await;
    assert!(ok && said.contains("held closed"), "{said}");
    let ll = l.clone();
    off(move || ll.wait_state("held")).await;
    let began = std::time::Instant::now();
    let told = http().get(l.url("/")).send().await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(told.status().as_u16(), 503);
    assert_eq!(
        told.headers()["content-type"].to_str().unwrap(),
        "text/plain; charset=utf-8"
    );
    let text = told.text().await.unwrap();
    assert!(
        text.contains("The link to devbox is held closed")
            && text.contains("spandeck link up devbox"),
        "{text}"
    );
    // The carrier is gone, and none is started while it is held.
    wait_for("the carrier to end", 10, || {
        (!std::path::Path::new(&format!("/proc/{third}")).exists() || !cfg!(target_os = "linux"))
            .then_some(())
    });
    assert_eq!(
        listening(l.near.daemon_pid()),
        BTreeSet::from([l.near.port, l.port])
    );
    let ll = l.clone();
    let (ok, list) = off(move || ll.cli(&["list"])).await;
    assert!(
        ok && list.contains("devbox") && list.contains("held"),
        "{list}"
    );
    // The far station forgot the near side's address with its helper.
    let ll = l.clone();
    off(move || {
        wait_for("the helper's leaving to be logged", 10, || {
            (ll.far.log("link.detached").len() == 3).then_some(())
        })
    })
    .await;
    assert_eq!(foreign_host(&l.far, l.port).await, 403);

    let ll = l.clone();
    off(move || {
        let (ok, said) = ll.cli(&["up", "devbox"]);
        assert!(ok, "{said}");
        ll.wait_state("up");
    })
    .await;
    assert_eq!(l.rpc(None, "station.info").await.0, 401);
    // The far station and its agent were never touched.
    let ll = l.clone();
    off(move || {
        assert_eq!(ll.far.pane(&pane)["state"], json!("working"));
        assert_eq!(ll.far.log("station.started").len(), 1);
    })
    .await;
}

/// "If no daemon is running it starts one detached from the SSH session
/// and waits for it ... and says so"; and "a background reconnect never
/// installs, updates or restarts anything": a far side whose `spandeck`
/// is not there is said plainly, and is not hammered.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons"]
async fn a_far_side_with_no_station_running_is_started_and_one_with_no_spandeck_is_said() {
    /// The station the helper started is not this test's child.
    struct Started(Station);
    impl Drop for Started {
        fn drop(&mut self) {
            let pid = std::fs::read_to_string(self.0.root.join("state/spandeckd.pid"));
            if let Some(pid) = pid.ok().and_then(|p| p.trim().parse::<u64>().ok()) {
                send(pid, "TERM");
            }
        }
    }
    let l = off(|| {
        let far = Station::not_started("far", "");
        let port = free_port();
        let missing = format!(
            "{}\n[[link]]\nname = \"nowhere\"\ncmd = [\"sh\", \"-c\", \"echo 'sh: 1: exec: spandeck: not found' >&2; exit 127\", \"--\"]\n",
            link_to(&far, port)
        );
        let near = Station::start_with("near", &missing, &[]);
        (Started(far), near, port)
    })
    .await;
    let (far, near, port) = (&l.0 .0, &l.1, l.2);
    let rows = wait_for("the far station to be started and linked", 40, || {
        let rows = near.call("link.list", json!({}));
        (rows[0]["state"] == json!("up")).then_some(rows)
    });
    assert_eq!(rows[0]["station"], json!("far"));
    // It runs by itself: a station of its own, answering on its own port.
    far.wait_ready();
    let page = http()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status().as_u16(), 200);

    // The other link: no spandeck there. Said, with what to do, and it
    // is not tried again and again.
    let row = wait_for("the missing far side to be said", 20, || {
        let row = near.call("link.list", json!({}))[1].clone();
        (row["state"] == json!("needs you")).then_some(row)
    });
    assert_eq!(row["name"], json!("nowhere"));
    let said = row["said"].as_str().unwrap();
    assert!(
        said.contains("nowhere has no spandeck where a link looks for it")
            && said.contains("A link never installs anything")
            && said.contains("spandeck link up nowhere"),
        "{said}"
    );
    // A port of its own was chosen and kept for it.
    let chosen = row["port"].as_u64().unwrap();
    assert!(chosen >= 17400 && chosen != u64::from(port), "{row}");
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(near.root.join("state/links/nowhere.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["port"], json!(chosen));
    let told = http()
        .get(format!("http://127.0.0.1:{chosen}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(told.status().as_u16(), 503);
    assert!(told.text().await.unwrap().contains("it needs you"));
}

/// The owner's use, in a real browser: the address `spandeck link open`
/// prints is opened, the browser is signed in without a code or a second
/// step, the address keeps nothing of the sign-in, and a pane of the far
/// station is on screen with what its agent printed.
#[test]
#[ignore = "merge tier: a browser"]
fn a_browser_opens_the_far_deck_through_the_link_and_sees_a_pane() {
    let l = linked();
    let pane = l.far.fake(
        json!({"steps": [{"say": "printed on the far side"}, {"work": 600_000}]}),
        json!({"name": "worker"}),
    );
    l.far.wait_text(&pane, "printed on the far side");
    let (url, _, _) = l.open();
    let page = Page::open_address(&url, 1400, 900);
    assert_eq!(
        page.js("[location.host, location.hash.includes('signin'), (localStorage.getItem('spandeck.token') || '').startsWith('dev.')]"),
        json!([format!("localhost:{}", l.port), false, true])
    );
    page.goto(&format!("#/s/far/p/{pane}"));
    page.terminal_face();
    page.wait(
        "what the far agent printed",
        "document.querySelector('.pane .xterm-rows')?.textContent.includes('printed on the far side')",
    );
    // The same address a second time signs nobody in.
    let again = Page::open_signed_out_at(&url, 1000, 700);
    assert_eq!(
        again.js("localStorage.getItem('spandeck.token')"),
        Value::Null
    );
}
