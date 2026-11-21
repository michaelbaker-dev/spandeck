//! A station that cannot download its release is supplied it by the fleet
//! (the owner's decision of 17 Nov 2026: "if the target machine can't get
//! it via git then it should ask the closest server to serve it up").
//!
//! The release host is the stand-in the update tests use. A station "cannot
//! reach" it when its config names an address nothing listens at.

mod common;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::fake_releases::{Releases, TAG};
use common::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `[update]` for a station whose release host does not answer.
const NO_HOST: &str = "[update]\nurl = \"http://127.0.0.1:9\"\nin_place = false\n";

fn host_at(releases: &Releases) -> String {
    format!("[update]\nurl = \"{}\"\nin_place = false\n", releases.url())
}

fn target() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-gnu"
        }
    )
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Something that is published as the release for this machine and is not
/// one: a station that is supplied it takes it, checks it, and then cannot
/// unpack it, so nothing is upgraded and the station under test lives on.
fn not_a_release() -> Vec<u8> {
    (0..300_000u32).map(|i| (i * 31 % 253) as u8).collect()
}

fn online(s: &Station, others: &[&str]) {
    wait_for("the stations to find each other", 30, || {
        let v = s.call("fleet.refresh", json!({}));
        others
            .iter()
            .all(|o| {
                v["stations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|x| x["station"] == json!(o) && x["state"] == json!("online"))
            })
            .then_some(())
    });
}

/// What the ship's log of a station says of releases got from the fleet.
fn got(s: &Station) -> Vec<Value> {
    s.log("station.update")
        .iter()
        .map(|e| e["data"].clone())
        .filter(|d| d["outcome"] == json!("got the release from the fleet"))
        .collect()
}

/// Ask a station to update, and wait for the notice that says how the
/// fleet road ended. Returns the answer and the notice's words.
fn update(s: &Station, params: Value) -> (Value, String) {
    let before = ended(s).len();
    let said = s.call("station.update", params);
    assert_eq!(said["fleet"], json!(true), "{said}");
    let line = wait_for("the update through the fleet to end", 60, || {
        let all = ended(s);
        (all.len() > before).then(|| all[0].clone())
    });
    (said, line)
}

/// The words each update through the fleet ended with, newest first.
fn ended(s: &Station) -> Vec<String> {
    s.log("station.update")
        .iter()
        .filter(|e| matches!(e["data"]["outcome"].as_str(), Some("done" | "failed")))
        .map(|e| e["data"]["text"].as_str().unwrap().to_string())
        .collect()
}

/// A stand-in for a station of the fleet, for what a real one never does:
/// it answers the calls a station looks at the fleet with, and the three
/// release calls as the test says.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Acts {
    /// A station from before releases were supplied: it does not say it can.
    Before,
    /// It holds the release, and stops answering after the first piece.
    Dies,
    /// It holds the release, and the bytes it hands over are not it.
    OtherBytes,
    /// It makes ready another version than the one it offered.
    OtherVersion,
    /// It gives another SHA-256 for the release than the others do.
    OtherSum,
    /// Its own running copy of a newer version, which is not a release.
    OwnCopy,
}

struct Standin {
    port: u16,
    acts: Arc<Mutex<Acts>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Standin {
    fn asked(&self, method: &str) -> usize {
        let asked = self.asked.lock().unwrap();
        asked.iter().filter(|m| *m == method).count()
    }

    fn act(&self, acts: Acts) {
        *self.acts.lock().unwrap() = acts;
        self.asked.lock().unwrap().clear();
    }

    /// `fleet`: the fleet's id, as a real station of it says it.
    fn start(port: u16, fleet: String, archive: Vec<u8>) -> Standin {
        use axum::routing::post;
        let acts = Arc::new(Mutex::new(Acts::Before));
        let asked = Arc::new(Mutex::new(vec![]));
        let (acts2, asked2) = (acts.clone(), asked.clone());
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let app = axum::Router::new().route(
            "/api/peer/rpc",
            post(move |axum::Json(body): axum::Json<Value>| {
                let (acts, asked, fleet, archive) =
                    (acts2.clone(), asked2.clone(), fleet.clone(), archive.clone());
                async move {
                    let acts = *acts.lock().unwrap();
                    let method = body["method"].as_str().unwrap_or_default().to_string();
                    asked.lock().unwrap().push(method.clone());
                    let p = &body["params"];
                    let caps = if acts == Acts::Before {
                        json!(["task_continue"])
                    } else {
                        json!(["task_continue", "release_supply"])
                    };
                    let sum = sha(&archive);
                    let result = match method.as_str() {
                        "station.hello" | "station.info" => json!({
                            "station": "foxtrot", "version": "0.4.10", "protocol": 2, "fleet": fleet,
                            "now_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
                            "caps": caps, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                            "panes": 0, "working": 0, "needs_you": 0, "web": [], "labels": [],
                        }),
                        "release.offer" => match acts {
                            Acts::OtherSum => json!({"have": "kept", "tag": TAG, "size": archive.len(), "sha256": "0".repeat(64)}),
                            Acts::OwnCopy => json!({"have": "own", "tag": "v99.0.0"}),
                            _ => json!({"have": "kept", "tag": TAG, "size": archive.len(), "sha256": sum}),
                        },
                        "release.prepare" => match acts {
                            Acts::OtherVersion => json!({"tag": "v0.0.1", "target": p["target"], "size": archive.len(), "sha256": sum, "source": "kept"}),
                            _ => json!({"tag": p["tag"], "target": p["target"], "size": archive.len(), "sha256": sum, "source": "kept"}),
                        },
                        "release.chunk" => {
                            let pieces = asked.lock().unwrap().iter().filter(|m| *m == "release.chunk").count();
                            if acts == Acts::Dies && pieces > 1 {
                                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                            let at = p["offset"].as_u64().unwrap() as usize;
                            // A first piece smaller than the whole, so that it can die after it.
                            let len = (p["len"].as_u64().unwrap() as usize).min(archive.len() - at).min(100_000);
                            let mut bytes = archive[at..at + len].to_vec();
                            if acts == Acts::OtherBytes {
                                bytes[0] ^= 0xff;
                            }
                            use base64::Engine;
                            json!({"data_b64": base64::engine::general_purpose::STANDARD.encode(bytes), "size": archive.len()})
                        }
                        _ => return axum::Json(json!({"error": {"code": "invalid", "message": "a stand-in"}})).into_response(),
                    };
                    use axum::response::IntoResponse;
                    axum::Json(json!({"result": result})).into_response()
                }
            }),
        );
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let _ = axum::serve(listener, app).await;
            });
        });
        Standin { port, acts, asked }
    }
}

/// What a station answers to "who are you" (asked as any device may).
fn hello(port: u16) -> Value {
    use std::io::{Read, Write};
    let body = r#"{"method":"station.hello","params":{},"from":"test"}"#;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST /api/peer/rpc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let json = &all[all.find("\r\n\r\n").unwrap() + 4..];
    let json = &json[json.find('{').unwrap()..=json.rfind('}').unwrap()];
    serde_json::from_str::<Value>(json).unwrap()["result"].clone()
}

/// A bug that happened (21 Nov 2026, twice): `spandeck update --station
/// rtx6000-wsl` said "did not answer within 5 s" and the station was never
/// updated. Its line is slow, the answer waited for the whole download,
/// and the download ended with the call that stopped waiting. The download
/// is the station's own work: a slow one is answered at once as begun, and
/// the station updates when it has it.
#[test]
#[cfg_attr(windows, ignore = "the release is a tar.gz made with sh")]
fn a_station_whose_download_is_slow_answers_at_once_and_still_updates() {
    let releases = Releases::start(true);
    releases.slow(7000);
    let (pa, pb) = (free_port(), free_port());
    let a = Station::in_fleet("alpha", pa, &[pb], &host_at(&releases));
    let b = Station::in_fleet("bravo", pb, &[pa], NO_HOST);
    online(&a, &["bravo"]);
    online(&b, &["alpha"]);
    let pane = a.fake(
        json!({"steps": [{"say": "before-update marker"}]}),
        json!({}),
    );
    a.wait_text(&pane, "before-update marker");
    let before = a.call("station.info", json!({}));
    let began = std::time::Instant::now();
    let said = b.call(
        "peer.call",
        json!({"station": "alpha", "method": "station.update", "params": {}}),
    );
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(
        (&said["begun"], &said["release"], &said["note"]),
        (
            &json!(true),
            &json!(TAG),
            &json!(format!(
                "alpha is downloading {TAG}. It updates by itself when the download is checked."
            ))
        ),
        "{said}"
    );
    // Asked again while it downloads: said, and nothing is begun twice.
    let again = a.try_call("station.update", json!({})).unwrap_err();
    assert!(
        again.message.contains("already getting a release"),
        "{}",
        again.message
    );
    wait_for("the updated image to answer", 60, || {
        let i = a.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert!(a.text(&pane).contains("before-update marker"));
    assert!(a.log("station.rolled_back").is_empty());
    let name = format!("spandeck-{TAG}-{}.tar.gz", target());
    assert_eq!(releases.asked(&name), 1);
}

/// Done when: a station whose release host does not answer is updated
/// through a station of its fleet that can reach it. The bytes are the
/// published release (the SHA-256 that station checked is the published
/// one, and what arrived has it), the station upgrades in place with its
/// terminal kept, passes its health check and is not rolled back; the
/// answer and one notice say what is happening; and `spandeck doctor`
/// there says its updates come through the fleet and who supplied the last.
#[test]
#[cfg_attr(windows, ignore = "the release is a tar.gz made with sh")]
fn a_station_that_cannot_reach_the_release_host_is_updated_through_the_fleet() {
    let releases = Releases::start(true);
    let (pa, pb) = (free_port(), free_port());
    let a = Station::in_fleet("alpha", pa, &[pb], NO_HOST);
    let b = Station::in_fleet("bravo", pb, &[pa], &host_at(&releases));
    online(&a, &["bravo"]);
    online(&b, &["alpha"]);
    let pane = a.fake(
        json!({"steps": [{"say": "before-update marker"}]}),
        json!({}),
    );
    a.wait_text(&pane, "before-update marker");
    let before = a.call("station.info", json!({}));
    let pane_pid = a.pane(&pane)["pid"].clone();
    assert!(before["caps"]
        .as_array()
        .unwrap()
        .contains(&json!("release_supply")));

    // Asked through bravo, as `spandeck update --station alpha` asks: the
    // answer comes at once and says the fleet is being asked.
    let said = b.call(
        "peer.call",
        json!({"station": "alpha", "method": "station.update", "params": {}}),
    );
    assert_eq!(
        (&said["fleet"], &said["note"]),
        (
            &json!(true),
            &json!("alpha cannot reach 127.0.0.1:9: asking the stations of your fleet for the release…")
        ),
        "{said}"
    );
    let after = wait_for("the updated image to answer", 60, || {
        let i = a.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_eq!(a.pane(&pane)["pid"], pane_pid);
    assert!(a.text(&pane).contains("before-update marker"));
    assert_eq!(after["dirs"]["program"], before["dirs"]["program"]);
    assert_eq!(after["version"], json!(env!("CARGO_PKG_VERSION")));
    assert!(a.log("station.rolled_back").is_empty());
    // What was taken: the published release, from bravo, by its SHA-256.
    let name = format!("spandeck-{TAG}-{}.tar.gz", target());
    let took = got(&a);
    assert_eq!(took.len(), 1, "{took:?}");
    assert_eq!(
        (&took[0]["from"], &took[0]["release"], &took[0]["what"]),
        (
            &json!("bravo"),
            &json!(TAG),
            &json!("the published release, checked by that station against its published SHA-256")
        ),
        "{took:?}"
    );
    let kept = b.root.join("state/supply").join(TAG);
    let published = std::fs::read(kept.join(format!("{}.tar.gz", target()))).unwrap();
    assert_eq!(took[0]["sha256"], json!(sha(&published)));
    assert_eq!(took[0]["bytes"], json!(published.len()));
    assert_eq!(releases.asked(&name), 1);
    // One notice, whose words were replaced as it went.
    let notes: Vec<Value> = a
        .call("notify.list", json!({"limit": 50}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == json!("station.update"))
        .cloned()
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0]["title"].as_str().unwrap().starts_with(&format!(
            "alpha cannot reach 127.0.0.1:9: got {TAG} from bravo ("
        )),
        "{notes:?}"
    );
    // Nothing of it is left in alpha's state, and alpha does not hand on
    // what it did not check against the release host itself.
    let left: Vec<_> = std::fs::read_dir(a.root.join("state/supply"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // `spandeck doctor` asks the release host (only it does), and says so.
    online(&a, &["bravo"]);
    let doctor = a.call("station.doctor", json!({"reach": true}));
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("release host"))
        .unwrap()
        .clone();
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("this station cannot reach the release host (127.0.0.1:9); updates come through the fleet (last supplied by bravo on ")
            && detail.ends_with(&format!(": {TAG}): `spandeck update` asks its stations")),
        "{detail}"
    );
}

/// Done when, for a station that cannot reach its release host (what is
/// published as the release here is not one, so every try ends at the
/// unpacking and the station lives on to be asked again):
///
/// - a station from before releases were supplied is never asked;
/// - of two stations that can supply it, the one that already holds it is
///   taken from, and the other downloads nothing;
/// - a station that stops answering under the transfer, one whose bytes do
///   not have the SHA-256 it offered, and one that makes ready another
///   version than it offered, are each passed over and the next is taken
///   from; the ship's log says why each was;
/// - two stations that give different SHA-256s for one release: nothing is
///   taken from either, and the notice says so;
/// - a station's own copy is taken only when no station holds or can
///   fetch a release, and the notice says it is that station's own copy;
/// - when no station of the fleet can supply releases the answer says so
///   at once;
/// - an agent cannot ask for a release, and neither can the owner's own
///   connection: it is a station's call.
#[test]
#[cfg_attr(windows, ignore = "the stand-in release is made for Unix names")]
fn the_closest_station_that_can_supplies_it_and_what_does_not_check_out_is_refused() {
    let file = format!("spandeck-{TAG}-{}.tar.gz", target());
    let archive = not_a_release();
    let releases = Releases::start_with(true, false, &[(file.clone(), archive.clone())]);
    let (pa, pb, pc, pf) = (free_port(), free_port(), free_port(), free_port());
    let b = Station::in_fleet("bravo", pb, &[pa, pc], &host_at(&releases));
    let c = Station::in_fleet("charlie", pc, &[pa, pb], &host_at(&releases));
    let fleet = hello(pb)["fleet"].as_str().unwrap().to_string();
    let f = Standin::start(pf, fleet, archive.clone());
    let a = Station::in_fleet("alpha", pa, &[pb, pc, pf], NO_HOST);
    online(&a, &["bravo", "charlie", "foxtrot"]);
    let reason = |s: &Station| -> Vec<(String, String)> {
        s.log("release.supply")
            .iter()
            .map(|e| {
                (
                    e["data"]["from"].as_str().unwrap().to_string(),
                    e["data"]["why"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    // alpha is told again what foxtrot says of itself.
    let acts = |acts: Acts| {
        f.act(acts);
        wait_for("alpha to look at foxtrot again", 30, || {
            a.call("fleet.refresh", json!({}));
            (f.asked("station.info") > 0).then_some(())
        });
    };

    // ---- charlie already holds it; bravo would have to fetch it. foxtrot
    // is from before releases were supplied, and is not asked.
    let kept = c.root.join("state/supply").join(TAG);
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join(format!("{}.tar.gz", target())), &archive).unwrap();
    std::fs::write(
        kept.join(format!("{}.sha256", target())),
        format!("{}\n", sha(&archive)),
    )
    .unwrap();
    let (_, line) = update(&a, json!({}));
    assert_eq!(
        line,
        format!("alpha cannot reach 127.0.0.1:9: got {TAG} from charlie, and it was not installed: the release could not be unpacked; nothing was changed")
    );
    let took = got(&a);
    assert_eq!(
        (took.len(), &took[0]["from"], &took[0]["sha256"]),
        (1, &json!("charlie"), &json!(sha(&archive))),
        "{took:?}"
    );
    assert_eq!(releases.asked(&file), 0);
    assert_eq!(f.asked("release.offer"), 0);
    assert!(f.asked("station.info") > 0);

    // ---- foxtrot holds it and stops answering after the first piece: the
    // next station is taken from. (charlie no longer holds it, so foxtrot
    // is the only one that does and is asked first.)
    std::fs::remove_dir_all(&kept).unwrap();
    acts(Acts::Dies);
    let (_, line) = update(&a, json!({}));
    let took = got(&a);
    assert_eq!(took.len(), 2, "{line}");
    assert!(
        took[0]["from"] == json!("bravo") || took[0]["from"] == json!("charlie"),
        "{took:?}"
    );
    let why = reason(&a);
    assert_eq!(why.len(), 1, "{why:?}");
    assert!(
        why[0].0 == "foxtrot" && why[0].1.starts_with("it stopped after 100 KB of 300 KB: "),
        "{why:?}"
    );
    assert!(f.asked("release.chunk") >= 3);

    // ---- foxtrot's bytes do not have the SHA-256 it offered; then it
    // makes ready another version than it offered. Each time it is passed
    // over and the release is taken from another station.
    for (acting, says) in [
        (
            Acts::OtherBytes,
            "what arrived does not have the SHA-256 it was offered with, so it was thrown away",
        ),
        (
            Acts::OtherVersion,
            &*format!(
                "it made ready v0.0.1 for {t}, which is not what was asked for ({TAG} for {t})",
                t = target()
            ),
        ),
    ] {
        // The stations that fetched it hold it now; foxtrot must be first.
        let _ = std::fs::remove_dir_all(b.root.join("state/supply"));
        let _ = std::fs::remove_dir_all(c.root.join("state/supply"));
        let (before, passed) = (got(&a).len(), reason(&a).len());
        acts(acting);
        let (_, line) = update(&a, json!({}));
        let took = got(&a);
        assert_eq!(took.len(), before + 1, "{acting:?}: {line}");
        assert_ne!(took[0]["from"], json!("foxtrot"), "{acting:?}");
        let why = reason(&a);
        assert_eq!(why.len(), passed + 1, "{acting:?}: {why:?}");
        assert_eq!(
            (why[0].0.as_str(), why[0].1.as_str()),
            ("foxtrot", says),
            "{acting:?}"
        );
    }
    let left: Vec<_> = std::fs::read_dir(a.root.join("state/supply"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // ---- foxtrot gives another SHA-256 for the release than the others:
    // nothing is taken from anyone.
    let before = got(&a).len();
    acts(Acts::OtherSum);
    let (_, line) = update(&a, json!({}));
    assert!(
        line.starts_with("alpha cannot reach 127.0.0.1:9: it was not updated, because ")
            && line.contains(&format!(" give different SHA-256s for {TAG}, so one of them holds something that is not that release, and nothing was taken from either."))
            && line.contains("foxtrot"),
        "{line}"
    );
    assert_eq!(got(&a).len(), before);
    assert_eq!(f.asked("release.prepare"), 0);

    // ---- A station's own copy is not taken while a station can fetch
    // the release...
    acts(Acts::OwnCopy);
    update(&a, json!({}));
    assert_ne!(got(&a)[0]["from"], json!("foxtrot"));
    assert_eq!(f.asked("release.prepare"), 0);
    // ...and is when none can: the release host is away for all of them,
    // and what bravo and charlie kept is gone.
    releases.down(true);
    let _ = std::fs::remove_dir_all(b.root.join("state/supply"));
    let _ = std::fs::remove_dir_all(c.root.join("state/supply"));
    let (_, line) = update(&a, json!({}));
    assert_eq!(
        line,
        "alpha cannot reach 127.0.0.1:9: got foxtrot's own copy, v99.0.0 from foxtrot, and it was not installed: the release could not be unpacked; nothing was changed"
    );
    assert_eq!(
        got(&a)[0]["what"],
        json!("that station's own program"),
        "{:?}",
        got(&a)
    );
    releases.down(false);

    // ---- An agent cannot ask for a release or be handed one, and it is
    // not the owner's call either: a station asks, for itself.
    let pane = b.fake(json!({"steps": []}), json!({}));
    for (method, params) in [
        ("release.offer", json!({"target": target()})),
        ("release.prepare", json!({"tag": TAG, "target": target()})),
        (
            "release.chunk",
            json!({"tag": TAG, "target": target(), "sha256": sha(&archive), "offset": 0, "len": 10}),
        ),
    ] {
        let err = b.call_as_pane(&pane, method, params.clone()).unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {}", err.message);
        let err = b.try_call(method, params).unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {}", err.message);
    }

    // ---- No station of the fleet can supply releases: said at once.
    drop(b);
    drop(c);
    acts(Acts::Before);
    wait_for("alpha to see that bravo and charlie are gone", 40, || {
        let v = a.call("fleet.refresh", json!({}));
        v["stations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["state"] == json!("online"))
            .all(|x| x["station"] == json!("alpha") || x["station"] == json!("foxtrot"))
            .then_some(())
    });
    let err = a.try_call("station.update", json!({})).unwrap_err();
    assert_eq!(
        err.message,
        "alpha cannot reach 127.0.0.1:9 to download the release, and no station of your fleet can supply releases yet (foxtrot run a spandeck from before stations could: update one of them first). Update it when it can reach 127.0.0.1:9, or copy a release archive to it and run `spandeck upgrade <the spandeck program in it>`."
    );
    let _ = f.port;
}

fn executable(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    spandeck_terminal::sys::set_executable(path).unwrap();
}

/// Done when: a station that is adding a machine of another kind, and
/// cannot reach the release host itself, gets that kind's release from a
/// station of its fleet that can, and sends it on over SSH; the dialog
/// says whose it is. (Before, the add ended with "add it from a station
/// that is the same kind of machine".) The machine is a stand-in Mac whose
/// `curl` reaches nothing; its release holds a stand-in `spandeck`.
#[test]
#[cfg_attr(windows, ignore = "the machine is a stand-in run by sh")]
fn a_station_adding_a_machine_gets_its_release_from_a_station_that_can_reach_the_host() {
    // Another kind of machine than this one, whichever this one is: a Mac
    // on Apple silicon, or, where the test itself runs on one, a Linux PC.
    let other = common::other_kind();
    let (kind_target, kind_words, kind_tailscale) = (other.target, other.words, other.tailscale);
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    for d in ["standin", "remote-bin", "pack", "mac/home", "mac/tmp"] {
        std::fs::create_dir_all(at(d)).unwrap();
    }
    let name = format!("spandeck-{TAG}-{kind_target}");
    std::fs::create_dir_all(at("pack").join(&name)).unwrap();
    for b in ["spandeck", "sd"] {
        executable(
            &at("pack").join(&name).join(b),
            "#!/bin/sh\ncase \"$1\" in --version) echo 'spandeck 9.9.9' ;; --json) echo '{ \"code\": \"K7QM2X\" }' ;; esac\nexit 0\n",
        );
    }
    assert!(std::process::Command::new("tar")
        .arg("-czf")
        .arg(at("mac.tar.gz"))
        .arg("-C")
        .arg(at("pack"))
        .arg(&name)
        .status()
        .unwrap()
        .success());
    let mac = std::fs::read(at("mac.tar.gz")).unwrap();
    let file = format!("{name}.tar.gz");
    let releases = Releases::start_with(true, false, &[(file.clone(), mac.clone())]);
    std::fs::write(
        at("standin/status.json"),
        json!({"Self": {"UserID": 7, "HostName": "alpha", "TailscaleIPs": ["100.64.0.1"]},
            "User": {"7": {"LoginName": "owner@example.com"}},
            "Peer": {"key-studio": {"ID": "n-studio", "HostName": "studio", "DNSName": "studio.tail0000.ts.net.",
                "OS": kind_tailscale, "UserID": 7, "Online": true, "TailscaleIPs": ["100.64.0.8"]}}})
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
    executable(
        &at("remote-bin/uname"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in -m) echo {} ;; *) echo {} ;; esac\n",
            other.uname.1, other.uname.0
        ),
    );
    executable(&at("remote-bin/curl"), "#!/bin/sh\nexit 7\n");
    executable(&at("remote-bin/sleep"), "#!/bin/sh\nexit 0\n");
    for tool in ["systemctl", "launchctl", "loginctl"] {
        executable(&at("remote-bin").join(tool), "#!/bin/sh\nexit 1\n");
    }
    executable(
        &at("standin/ssh"),
        &format!(
            "#!/bin/sh\nexec env -i PATH={d}/remote-bin:/usr/bin:/bin HOME={d}/mac/home TMPDIR={d}/mac/tmp SPANDECK_BIN_DIR={d}/mac/bin sh -s\n",
            d = dir.path().display()
        ),
    );
    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let stations = Station::fleet(&["alpha", "bravo"], |name, _| match name {
        "alpha" => (
            format!(
                "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\n{NO_HOST}",
                at("standin/ssh").display()
            ),
            vec![("PATH".to_string(), path.clone())],
        ),
        _ => (host_at(&releases), vec![]),
    });
    let (a, b) = (&stations[0], &stations[1]);
    online(a, &["bravo"]);
    online(b, &["alpha"]);
    a.call("fleet.add_enable", json!({"on": true}));
    a.call("fleet.add", json!({"machine": "n-studio", "user": "sam"}));
    let v = wait_for_or(
        "the add to come to rest",
        60,
        || a.call("fleet.add_status", json!({})).to_string(),
        || {
            let v = a.call("fleet.add_status", json!({}));
            (v["running"] == json!(false)).then_some(v)
        },
    );
    let said = v["lines"].to_string();
    for piece in [
        format!("studio cannot reach 127.0.0.1:9. Getting spandeck for {kind_words} on alpha to send it over SSH…"),
        format!("studio cannot reach 127.0.0.1:9, and neither can alpha: sending the release bravo fetched and checked, {TAG}, from alpha over SSH (1 KB)…"),
        format!("Received {} bytes, and their SHA-256 matches.", mac.len()),
        format!("Installed spandeck {TAG} to "),
    ] {
        assert!(said.contains(&piece), "{piece:?} is not in: {v}");
    }
    assert_eq!(
        v["problem"]["worked"],
        json!("spandeck 9.9.9 was installed on studio and is running."),
        "{v}"
    );
    assert!(at("mac/bin/spandeck").exists());
    assert_eq!(releases.asked(&file), 1);
    // The ship's log says whose it was.
    let sent = a
        .log("fleet.add")
        .iter()
        .map(|e| e["data"].clone())
        .find(|d| d["outcome"] == json!("sending a release over SSH"))
        .expect("the sending is in the ship's log");
    assert_eq!(
        (&sent["via"], &sent["from"]),
        (
            &json!("bravo"),
            &json!("another station of the fleet, which checked it against its published SHA-256")
        ),
        "{sent}"
    );
}

/// Done when: with no station able to reach the release host, a station
/// asked for a version by name takes the running program of a station of
/// its own kind that runs that version, upgrades to it in place and is not
/// rolled back, and its notice says it was that station's own copy. And
/// while that transfer (the real program, tens of megabytes) is under
/// way, ordinary calls between the two stations are answered as quickly
/// as ever: the pieces travel on connections of their own. The bound is
/// half a second for every call; they take a few milliseconds.
#[test]
#[ignore = "merge tier: packs and moves the real program, and times calls (about half a minute)"]
fn a_stations_own_copy_updates_another_and_calls_between_them_are_not_held_up() {
    let (pa, pb) = (free_port(), free_port());
    let a = Station::in_fleet("alpha", pa, &[pb], NO_HOST);
    let b = Station::in_fleet("bravo", pb, &[pa], NO_HOST);
    online(&a, &["bravo"]);
    online(&b, &["alpha"]);
    let before = a.call("station.info", json!({}));
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let said = a.call("station.update", json!({"version": version}));
    assert_eq!(said["fleet"], json!(true), "{said}");
    // bravo asks alpha, and alpha asks bravo, again and again until alpha
    // has the release. The last round may meet the upgrade and is left out.
    let mut took = vec![];
    loop {
        let began = std::time::Instant::now();
        let to_a = b.try_call(
            "peer.call",
            json!({"station": "alpha", "method": "station.info", "params": {}}),
        );
        let to_b = a.try_call(
            "peer.call",
            json!({"station": "bravo", "method": "station.info", "params": {}}),
        );
        let round = began.elapsed();
        let has_it = a
            .try_call("log.query", json!({"kind": "station.update"}))
            .ok()
            .is_none_or(|l| {
                l.as_array().is_some_and(|l| {
                    l.iter().any(|e| {
                        matches!(
                            e["data"]["outcome"].as_str(),
                            Some("got the release from the fleet" | "failed" | "done")
                        )
                    })
                })
            });
        if has_it {
            break;
        }
        assert!(to_a.is_ok() && to_b.is_ok(), "{to_a:?} {to_b:?}");
        took.push(round);
        std::thread::sleep(Duration::from_millis(20));
    }
    let after = wait_for("the updated image to answer", 90, || {
        let i = a.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_eq!(after["version"], json!(env!("CARGO_PKG_VERSION")));
    assert!(a.log("station.rolled_back").is_empty());
    let got = got(&a);
    assert_eq!(
        (&got[0]["from"], &got[0]["what"]),
        (&json!("bravo"), &json!("that station's own program")),
        "{got:?}"
    );
    let line = wait_for("the notice of how it went", 30, || {
        ended(&a).first().cloned()
    });
    assert!(
        line.starts_with(&format!(
            "alpha cannot reach 127.0.0.1:9: got bravo's own copy, {version} from bravo ("
        )),
        "{line}"
    );
    // What bravo put together for it is not kept.
    wait_for("bravo's own copy to be gone", 20, || {
        std::fs::read_dir(b.root.join("state/supply"))
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .is_none()
            .then_some(())
    });
    took.sort();
    assert!(took.len() >= 5, "only {} rounds were timed", took.len());
    println!(
        "{} rounds of two calls during the transfer: median {:?}, slowest {:?}",
        took.len(),
        took[took.len() / 2],
        took[took.len() - 1]
    );
    assert!(
        took[took.len() - 1] < Duration::from_millis(1000),
        "a round of two calls took {:?}",
        took[took.len() - 1]
    );
}
