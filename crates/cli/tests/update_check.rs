//! Is there a newer spandeck? (The owner, 19 Nov 2026: "how do I know if
//! there is a spandeck update?"; DECISIONS.md, "Is there a newer spandeck?")
//!
//! The release host here is a listener on this machine that answers
//! `/latest` with a redirect, as the real one does. No test asks the real
//! one: every station of the harness has the check off unless its test
//! says `check = true` and names this stand-in.

mod common;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{json, Value};

const OURS: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct Said {
    /// Where `/latest` redirects to.
    location: String,
    /// Answers "unavailable" instead.
    down: bool,
    /// Every request, as it arrived.
    requests: Vec<String>,
}

/// A stand-in for the release host.
struct Host {
    port: u16,
    said: Arc<Mutex<Said>>,
}

impl Host {
    fn start() -> Host {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let said = Arc::new(Mutex::new(Said::default()));
        let theirs = said.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let (mut request, mut buf) = (Vec::new(), [0u8; 2048]);
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(n) if n > 0 => request.extend_from_slice(&buf[..n]),
                        _ => break,
                    }
                }
                let answer = {
                    let mut said = theirs.lock().unwrap();
                    said.requests
                        .push(String::from_utf8_lossy(&request).to_string());
                    if said.down {
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    } else {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            said.location
                        )
                    }
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Host { port, said }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// `[update]` for a station that takes its releases from here and asks.
    fn config(&self) -> String {
        format!(
            "[update]\nurl = \"{}\"\ncheck = true\nin_place = false\n",
            self.url()
        )
    }

    /// From now on the newest release is this one.
    fn newest(&self, version: &str) {
        self.redirects_to(&format!("/tag/v{version}"));
    }

    fn redirects_to(&self, location: &str) {
        let mut said = self.said.lock().unwrap();
        said.location = location.to_string();
        said.down = false;
    }

    fn down(&self) {
        self.said.lock().unwrap().down = true;
    }

    fn requests(&self) -> Vec<String> {
        self.said.lock().unwrap().requests.clone()
    }
}

/// Ask now, and come back with what the station says of itself once the
/// host has been asked once more.
fn check(s: &Station, host: &Host) -> Value {
    let before = host.requests().len();
    let asked = s.call("update.check", json!({"again": true}));
    assert_eq!(asked["off"], json!(false), "{asked}");
    wait_for("the station to have asked its release host", 15, || {
        let update = s.call("station.info", json!({}))["update"].clone();
        (host.requests().len() > before && update["checking"] != json!(true)).then_some(update)
    })
}

/// What the fleet's list says: the fleet's view, and this station's row.
fn view(s: &Station) -> (Value, Value) {
    let v = s.call("fleet.view", json!({}));
    let row = v["stations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"] == json!(s.name))
        .unwrap()
        .clone();
    (v["update"].clone(), row)
}

fn cli(s: &Station, args: &[&str]) -> String {
    let out = s
        .command(SPANDECK)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The request: a release newer than the station's is said with its
/// version and a link to its page; the same one is "latest"; a host that
/// does not answer is "could not check" with the reason, and what an
/// earlier good answer said is not turned into "latest"; what the host
/// sends that is not a version is thrown away, and the link is never an
/// address the host gave. The request itself says nothing of the station.
#[test]
fn a_newer_release_is_said_and_a_failed_check_is_never_latest() {
    let host = Host::start();
    host.newest(OURS);
    let s = Station::start_with("solo", &host.config(), &[]);

    // Nothing was asked by starting, and nothing is claimed.
    let (fleet, row) = view(&s);
    assert_eq!(fleet["state"], "not_checked", "{fleet}");
    assert_eq!(row["update"]["says"], "not checked yet");
    assert!(host.requests().is_empty());

    // The same release as the station runs: latest.
    let own = check(&s, &host);
    assert_eq!(own["state"], "latest", "{own}");
    let (fleet, row) = view(&s);
    assert_eq!(fleet["state"], "latest", "{fleet}");
    assert!(fleet["url"].is_null());
    assert_eq!(row["update"]["says"], "latest");
    // One plain GET, with nothing of the station, its owner or a project.
    let requests = host.requests();
    assert_eq!(requests.len(), 1);
    let request = requests[0].to_lowercase();
    assert!(request.starts_with("get /latest http/1.1\r\n"), "{request}");
    let headers: Vec<&str> = request
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':').map(|(name, _)| name))
        .collect();
    assert!(
        headers
            .iter()
            .all(|h| ["host", "user-agent", "accept"].contains(h)),
        "{request}"
    );
    assert!(request.contains("user-agent: spandeck\r\n"), "{request}");
    assert!(!request.contains("solo") && !request.contains(OURS));
    // An answer is kept a day: asking without "again" asks nobody.
    let kept = s.call("update.check", json!({}));
    assert_eq!(kept["asking"], json!(false), "{kept}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(host.requests().len(), 1);

    // The host stops answering: "could not check", and not "latest".
    host.down();
    let own = check(&s, &host);
    assert_eq!(own["state"], "unknown", "{own}");
    let (fleet, row) = view(&s);
    assert_eq!(fleet["state"], "unknown", "{fleet}");
    assert_eq!(fleet["says"], "could not check: 127.0.0.1 answered 503");
    assert!(fleet["version"].is_null() && fleet["url"].is_null());
    assert_eq!(
        row["update"]["says"],
        "could not check: 127.0.0.1 answered 503"
    );
    assert!(cli(&s, &["fleet"]).contains(
        "Whether a newer spandeck is published: could not check: 127.0.0.1 answered 503"
    ));

    // A hostile answer: markup where the version would be.
    host.redirects_to("https://evil.example/tag/v99.9.9<img src=x onerror=alert(1)>");
    let own = check(&s, &host);
    assert_eq!(own["state"], "unknown", "{own}");
    assert_eq!(own["why"], "127.0.0.1 did not answer with a release's name");
    let all = s.call("fleet.view", json!({})).to_string();
    for never in ["evil", "<img", "99.9.9", "onerror"] {
        assert!(!all.contains(never), "{never}: {all}");
    }

    // A newer release, named by a redirect to somebody else's address:
    // the version is taken, and the link is this station's own release
    // address with it, not the address in the answer.
    host.redirects_to("https://evil.example/releases/tag/v99.1.2?next=javascript:alert(1)");
    let own = check(&s, &host);
    assert_eq!(own["state"], "available", "{own}");
    let (fleet, row) = view(&s);
    assert_eq!(fleet["state"], "available", "{fleet}");
    assert_eq!(fleet["version"], "99.1.2");
    assert_eq!(fleet["says"], "spandeck 99.1.2 is available");
    assert_eq!(fleet["url"], json!(format!("{}/tag/v99.1.2", host.url())));
    assert_eq!(fleet["behind"], json!(["solo"]));
    assert_eq!(row["update"]["says"], "99.1.2 available");
    let all = s.call("fleet.view", json!({})).to_string();
    assert!(
        !all.contains("evil") && !all.contains("javascript"),
        "{all}"
    );
    // The same from the command line, which asks once more.
    let said = cli(&s, &["update", "--check"]);
    assert!(said.contains("99.1.2 available"), "{said}");
    assert!(
        said.contains(&format!(
            "spandeck 99.1.2 is available: {}/tag/v99.1.2.",
            host.url()
        )),
        "{said}"
    );
    // Nothing was asked for but the newest release's name: no download.
    assert!(host
        .requests()
        .iter()
        .all(|r| r.starts_with("GET /latest HTTP/1.1\r\n")));
    assert_eq!(
        s.call("station.info", json!({}))["version"],
        json!(OURS),
        "checking installs nothing"
    );

    // The host goes away again: the release it named is still published.
    host.down();
    let own = check(&s, &host);
    assert_eq!(own["state"], "available", "{own}");
    assert_eq!(own["why"], "127.0.0.1 answered 503");
    assert_eq!(view(&s).0["version"], "99.1.2");
}

/// The request: a switch turns it off, in the config file and the way the
/// other switches are set, and then nothing is asked: the host sees no
/// request, and the deck is told checks are off.
#[test]
fn with_the_switch_off_nothing_is_asked() {
    let host = Host::start();
    host.newest("99.1.2");
    // In the config file.
    let off = Station::start_with(
        "quiet",
        &format!("[update]\nurl = \"{}\"\ncheck = false\n", host.url()),
        &[],
    );
    let asked = off.call("update.check", json!({"again": true}));
    assert_eq!(asked["off"], json!(true), "{asked}");
    let (fleet, row) = view(&off);
    assert_eq!(fleet["state"], "off", "{fleet}");
    assert_eq!(fleet["says"], "checks are off");
    assert_eq!(row["update"]["says"], "checks are off");
    assert!(cli(&off, &["fleet"]).contains("checks are off"));

    // By the switch, while the station runs; and on again.
    let s = Station::start_with("asks", &host.config(), &[]);
    s.call("station.flags", json!({"set": {"update_check": false}}));
    let asked = s.call("update.check", json!({"again": true}));
    assert_eq!(asked["off"], json!(true), "{asked}");
    assert_eq!(view(&s).0["state"], "off");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(host.requests().is_empty(), "{:?}", host.requests());
    s.call("station.flags", json!({"set": {"update_check": null}}));
    assert_eq!(check(&s, &host)["state"], "available");
    assert_eq!(host.requests().len(), 1);
}

/// The request: a station with no way out has no answer of its own, and
/// the fleet's view is the newest any station learned. Two stations, of
/// which only one reaches the release host: both decks say it.
#[test]
#[ignore = "merge tier: two stations"]
fn one_station_that_reaches_the_host_is_enough_for_both_decks() {
    let host = Host::start();
    host.newest("99.1.2");
    let (pa, pb) = (free_port(), free_port());
    let a = Station::in_fleet("alpha", pa, &[pb], &host.config());
    let b = Station::in_fleet(
        "bravo",
        pb,
        &[pa],
        "[update]\nurl = \"http://127.0.0.1:9\"\ncheck = true\n",
    );
    wait_for("the two stations to see each other", 30, || {
        (a.sees("bravo").as_deref() == Some("online")
            && b.sees("alpha").as_deref() == Some("online"))
        .then_some(())
    });
    assert_eq!(check(&a, &host)["state"], "available");
    b.call("update.check", json!({"again": true}));
    let own = wait_for("bravo to have failed to reach its host", 20, || {
        let u = b.call("station.info", json!({}))["update"].clone();
        (u["state"] == json!("unknown")).then_some(u)
    });
    assert_eq!(own["why"], "no route to 127.0.0.1", "{own}");
    for deck in [&a, &b] {
        let fleet = wait_for("the deck to say a release is out", 30, || {
            let v = deck.call("fleet.refresh", json!({}));
            let said = v["stations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["update"]["says"] == json!("99.1.2 available"));
            (said && v["update"]["state"] == json!("available")).then_some(v["update"].clone())
        });
        assert_eq!(fleet["version"], "99.1.2", "{fleet}");
        assert_eq!(fleet["by"], "alpha", "{fleet}");
        assert_eq!(fleet["behind"].as_array().unwrap().len(), 2, "{fleet}");
        // The link is each station's own release address, never one
        // another station gave it.
        let own = if deck.name == "alpha" {
            host.url()
        } else {
            "http://127.0.0.1:9".to_string()
        };
        assert_eq!(fleet["url"], json!(format!("{own}/tag/v99.1.2")));
    }
    // Only alpha's host was asked, and bravo asked nobody else.
    assert!(host
        .requests()
        .iter()
        .all(|r| r.starts_with("GET /latest HTTP/1.1\r\n")));
}

/// The request: the deck shows one quiet line when a newer release is
/// out, with the version as text and a link to the release's page; Update
/// asks before anything is done; "Not now" is remembered for that version
/// in this browser; and on a phone it is one line.
#[test]
#[ignore = "merge tier: a browser"]
fn the_deck_says_a_newer_release_is_out() {
    use common::browser::Page;
    let host = Host::start();
    host.newest("99.1.2");
    let s = Station::start_with("solo", &host.config(), &[]);
    assert_eq!(check(&s, &host)["state"], "available");
    let page = Page::open(&s, 1200, 800);
    let line = "document.querySelector('.update-line')";
    page.wait("the line", &format!("!!{line}"));
    assert_eq!(
        page.js(&format!("{line}.querySelector('.grow').innerText")),
        json!("spandeck 99.1.2 is available.")
    );
    // The version is text in a link to this station's release address,
    // opened apart from the deck.
    assert_eq!(
        page.js(&format!("(a => [a.textContent, a.href, a.target, a.rel, a.children.length])({line}.querySelector('a'))")),
        json!(["spandeck 99.1.2", format!("{}/tag/v99.1.2", host.url()), "_blank", "noopener", 0])
    );
    // Beside the station's version, where stations are listed.
    assert_eq!(
        page.js("document.querySelector('.tile.station .tile-version').innerText"),
        json!(format!("{OURS} · 99.1.2 available"))
    );
    // Update asks first, and Cancel does nothing.
    assert_eq!(
        page.js(&format!("{line}.querySelector('.update-all').innerText")),
        json!("Update solo")
    );
    page.js(&format!("{line}.querySelector('.update-all').click()"));
    page.wait(
        "the question",
        "document.querySelector('.dialog')?.innerText.includes('Update solo?')",
    );
    page.js("[...document.querySelectorAll('.dialog .row.end button')].find(b => b.innerText === 'Cancel').click()");
    assert!(host
        .requests()
        .iter()
        .all(|r| r.starts_with("GET /latest HTTP/1.1\r\n")));
    // On a phone: one line, inside the screen.
    page.phone_screen();
    page.wait(
        "the line on a phone",
        &format!("{line}?.querySelector('.update-all')?.innerText === 'Update'"),
    );
    assert_eq!(
        page.js(&format!("(r => r.height <= 52 && r.right <= innerWidth && document.documentElement.scrollWidth <= innerWidth)({line}.getBoundingClientRect())")),
        json!(true),
        "{}",
        page.js(&format!("JSON.stringify({line}.getBoundingClientRect())"))
    );
    // Not now: gone, and still gone when the page is loaded again.
    page.js(&format!("{line}.querySelector('.update-later').click()"));
    page.wait("the line to go", &format!("!{line}"));
    page.reload();
    page.wait(
        "the fleet's list",
        "window.__spandeck_state?.fleet?.update?.state === 'available' || !!document.querySelector('.tile-version, .home-switch')",
    );
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(page.js(&format!("!{line}")), json!(true));
    // A release after that one is said again.
    host.newest("99.2.0");
    assert_eq!(check(&s, &host)["version"], "99.2.0");
    page.wait(
        "the line for the next release",
        &format!("{line}?.innerText.includes('spandeck 99.2.0 is available.')"),
    );
}
