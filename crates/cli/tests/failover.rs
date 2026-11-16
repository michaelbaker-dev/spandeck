//! HTTPS on each station, and a page carrying on through another station
//! (DECISIONS.md, "HTTPS on every station, the app saved on the device,
//! and carrying on through another station", 17 Nov 2026).
//!
//! The stations here are on one machine, so their HTTPS name is
//! `localhost` and each is given a certificate made in the test, by a
//! command that stands in for `tailscale cert` (`[web] cert_cmd`). No test
//! asks Tailscale for anything.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// A certificate for `localhost` and its key, in one file, as `tailscale
/// cert --cert-file - --key-file -` prints them. `days`: how long it lasts.
fn make_cert(file: &Path, days: i64) -> String {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(days);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    std::fs::write(file, format!("{}{}", cert.pem(), key.serialize_pem())).unwrap();
    cert.pem()
}

/// The config of a station that serves HTTPS with the certificate in `file`.
fn https_config(file: &Path, port: u16) -> String {
    let print = if spandeck_terminal::sys::WINDOWS {
        format!(r#"["cmd", "/c", "type", '{}']"#, file.display())
    } else {
        format!(r#"["cat", '{}']"#, file.display())
    };
    format!("[web]\nhttps_port = {port}\nname = \"localhost\"\ncert_cmd = {print}\n")
}

struct Fleet {
    stations: Vec<Station>,
    /// Each station's HTTPS port (0: it serves none) and certificate file.
    https: Vec<(u16, PathBuf)>,
    certs: Vec<String>,
}

impl Fleet {
    /// Stations of one fleet. `shape`: "https" serves both; "http" is this
    /// version with no certificate; "old" announces and accepts nothing
    /// new, as a station on 0.4.10.
    fn start(shapes: &[(&str, &str)]) -> Fleet {
        let ports: Vec<u16> = shapes.iter().map(|_| free_port()).collect();
        let names: Vec<&str> = shapes.iter().map(|(n, _)| *n).collect();
        let (https, certs) = (
            std::cell::RefCell::new(vec![]),
            std::cell::RefCell::new(vec![]),
        );
        let stations = Station::fleet(&names, |name, root| {
            let i = names.iter().position(|n| *n == name).unwrap();
            let file = root.join("cert-and-key.pem");
            let extra = match shapes[i].1 {
                "https" => {
                    certs.borrow_mut().push(make_cert(&file, 60));
                    https.borrow_mut().push((ports[i], file.clone()));
                    https_config(&file, ports[i])
                }
                "old" => {
                    https.borrow_mut().push((0, file));
                    "[flags]\npage_failover = false\n".to_string()
                }
                _ => {
                    https.borrow_mut().push((0, file));
                    String::new()
                }
            };
            (extra, vec![])
        });
        for s in &stations {
            s.wait_ready();
        }
        Fleet {
            stations,
            https: https.into_inner(),
            certs: certs.into_inner(),
        }
    }

    fn https_origin(&self, i: usize) -> String {
        format!("https://localhost:{}", self.https[i].0)
    }

    fn http_origin(&self, i: usize) -> String {
        format!("http://127.0.0.1:{}", self.stations[i].port)
    }

    /// A client that trusts the stations' certificates and nothing else.
    fn client(&self) -> reqwest::Client {
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(10));
        for pem in &self.certs {
            b = b.add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap());
        }
        b.build().unwrap()
    }

    /// Every station lists every other as on, with what it says of HTTPS.
    fn settle(&self) {
        for (i, s) in self.stations.iter().enumerate() {
            if self.https[i].0 != 0 {
                wait_for(&format!("{} serving HTTPS", s.name), 20, || {
                    (s.call("station.info", json!({}))["https"]["on"] == json!(true)).then_some(())
                });
            }
        }
        for s in &self.stations {
            wait_for(&format!("{} seeing the fleet", s.name), 30, || {
                let view = s.call("fleet.refresh", json!({}));
                let all = view["stations"].as_array().unwrap();
                all.iter()
                    .all(|x| {
                        let i = self
                            .stations
                            .iter()
                            .position(|o| json!(o.name) == x["station"]);
                        x["state"] == json!("online")
                            && i.is_some_and(|i| {
                                (self.https[i].0 != 0) == (x["https"]["on"] == json!(true))
                            })
                    })
                    .then_some(())
            });
        }
    }
}

/// A browser on another device, signed in at `s`: its token.
async fn device_token(s: &Station) -> String {
    let http = reqwest::Client::new();
    let post = |path: &'static str, body: Value| {
        let (http, url) = (http.clone(), s.url(path));
        async move {
            let answer = http.post(url).json(&body).send().await.unwrap();
            answer.json::<Value>().await.unwrap()["result"].clone()
        }
    };
    let asked = post("/api/device/request", json!({"name": "the test phone"})).await;
    tokio::task::block_in_place(|| s.call("trust.approve", json!({"request": asked["code"]})));
    let collect = json!({"id": asked["id"], "collect": asked["collect"]});
    let got = post("/api/device/collect", collect).await;
    got["token"].as_str().expect("a token").to_string()
}

/// Open a socket at `url` as a page loaded from `origin` would, and sign
/// in with `token`. `Err`: the status the station refused the socket
/// with. `Ok`: the station's first answer.
async fn socket(url: &str, origin: Option<&str>, token: Option<&str>) -> Result<Value, u16> {
    let mut req = url.into_client_request().unwrap();
    if let Some(origin) = origin {
        req.headers_mut().insert("origin", origin.parse().unwrap());
    }
    let (mut ws, _) = match tokio_tungstenite::connect_async(req).await {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            return Err(resp.status().as_u16())
        }
        Err(e) => panic!("{url}: {e}"),
    };
    if let Some(token) = token {
        let auth = json!({"auth": {"token": token, "client": "web-test-1"}});
        ws.send(Message::Text(auth.to_string().into()))
            .await
            .unwrap();
    }
    match ws.next().await {
        Some(Ok(Message::Text(t))) => Ok(serde_json::from_str(&t).unwrap()),
        other => panic!("{url}: no first message: {other:?}"),
    }
}

/// Reason: DECISIONS.md, part 1 ("What is served where", "Where the key
/// lives", "What a station says", "No HSTS", "Renewal") and part 3 ("What
/// 'working through another station' is"); and Guarantees, Trust: "A
/// station removed from the fleet can no longer act on the stations that
/// took the new key". Three stations: two serve HTTPS, one answers as a
/// station on 0.4.10 does.
#[tokio::test(flavor = "multi_thread")]
async fn a_station_serves_https_and_takes_a_members_deck_as_an_origin() {
    let fleet = tokio::task::spawn_blocking(|| {
        let fleet = Fleet::start(&[("alpha", "https"), ("bravo", "https"), ("zulu", "old")]);
        fleet.settle();
        fleet
    })
    .await
    .unwrap();
    let (alpha, bravo, zulu) = (&fleet.stations[0], &fleet.stations[1], &fleet.stations[2]);
    let block = |f: &dyn Fn() -> Value| tokio::task::block_in_place(f);

    // --- HTTPS beside HTTP, and what a station says of it.
    let info = block(&|| alpha.call("station.info", json!({})));
    let https = &info["https"];
    assert_eq!(
        (
            &https["on"],
            &https["name"],
            &https["port"],
            &https["state"]
        ),
        (
            &json!(true),
            &json!("localhost"),
            &json!(fleet.https[0].0),
            &json!("on")
        ),
        "{https}"
    );
    assert!(
        (58..=60).contains(&https["days_left"].as_i64().unwrap()),
        "{https}"
    );
    let caps = |v: &Value| v["caps"].as_array().unwrap().clone();
    assert!(caps(&info).contains(&json!("https")) && caps(&info).contains(&json!("deck_via")));
    assert!(!info.to_string().contains("PRIVATE KEY"));
    // The station that is as one from before says neither, and is "off".
    let old = block(&|| zulu.call("station.info", json!({})));
    assert!(!caps(&old).contains(&json!("https")) && !caps(&old).contains(&json!("deck_via")));
    assert_eq!(old["https"]["on"], json!(false));

    let client = fleet.client();
    let page = client
        .get(format!("{}/", fleet.https_origin(0)))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    // No HSTS: it would turn every http address of the machine into https.
    assert!(page.headers().get("strict-transport-security").is_none());
    assert!(page.text().await.unwrap().contains("<div id=\"app\">"));
    // The API over HTTPS, with the station's own token, as over HTTP.
    let rpc = |origin: String, at: Option<&str>, token: String, method: &'static str| {
        let (client, at) = (client.clone(), at.map(String::from));
        async move {
            let mut req = client
                .post(format!("{origin}/api/rpc"))
                .bearer_auth(token)
                .json(&json!({"method": method, "params": {}}));
            if let Some(at) = at {
                req = req.header("origin", at);
            }
            req.send().await.unwrap().status().as_u16()
        }
    };
    assert_eq!(
        rpc(fleet.https_origin(0), None, alpha.token(), "station.info").await,
        200
    );
    assert_eq!(
        rpc(fleet.http_origin(0), None, alpha.token(), "station.info").await,
        200
    );
    // On the TLS port a page's origin must be https.
    let plain = format!("http://localhost:{}", fleet.https[0].0);
    assert_eq!(
        rpc(
            fleet.https_origin(0),
            Some(&plain),
            alpha.token(),
            "station.info"
        )
        .await,
        403
    );
    assert_eq!(
        rpc(
            fleet.https_origin(0),
            Some(&fleet.https_origin(0)),
            alpha.token(),
            "station.info"
        )
        .await,
        200
    );
    // The key: a copy only this user can read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tls = alpha.root.join("state/tls");
        assert_eq!(
            std::fs::metadata(&tls).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(tls.join("key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    // --- Another member's deck as the origin: `/api/ws` and `/api/ping`.
    let token = device_token(alpha).await;
    let ws = |s: &Station, path: &str| format!("ws://127.0.0.1:{}{path}", s.port);
    for origin in [
        fleet.http_origin(0),
        fleet.https_origin(0),
        fleet.http_origin(2),
    ] {
        let hello = socket(&ws(bravo, "/api/ws"), Some(&origin), Some(&token))
            .await
            .unwrap();
        let from = if origin == fleet.http_origin(2) {
            "zulu"
        } else {
            "alpha"
        };
        assert_eq!(
            (&hello["auth"]["ok"], &hello["auth"]["via"]),
            (&json!(true), &json!(from)),
            "{origin}: {hello}"
        );
    }
    let logged = block(&|| json!(bravo.log("deck.via")));
    assert!(
        logged
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["data"]["from"] == json!("alpha")),
        "{logged}"
    );
    // Only a browser's own token: not the station's, not a pane's.
    let pane = block(&|| json!(bravo.fake(json!([]), json!({}))));
    let pane_token = tokio::task::block_in_place(|| bravo.pane_token(pane.as_str().unwrap()));
    for wrong in [
        bravo.token(),
        alpha.token(),
        pane_token,
        "dev.nobody.00".to_string(),
    ] {
        let hello = socket(
            &ws(bravo, "/api/ws"),
            Some(&fleet.http_origin(0)),
            Some(&wrong),
        )
        .await
        .unwrap();
        assert_eq!(hello["auth"]["ok"], json!(false), "{hello}");
        assert!(
            hello["auth"]["message"]
                .as_str()
                .unwrap()
                .contains("only a signed-in browser's own token"),
            "{hello}"
        );
    }
    // The same tokens are taken from this station's own deck, as ever.
    let own = socket(
        &ws(bravo, "/api/ws"),
        Some(&fleet.http_origin(1)),
        Some(&bravo.token()),
    )
    .await
    .unwrap();
    assert_eq!(
        (&own["auth"]["ok"], &own["auth"]["via"]),
        (&json!(true), &Value::Null),
        "{own}"
    );
    // Not a member's deck: refused before anything is said.
    let stranger = "http://127.0.0.1:9";
    let other_port = format!("https://localhost:{}", alpha.port);
    for origin in [stranger, "https://example.com", other_port.as_str()] {
        assert_eq!(
            socket(&ws(bravo, "/api/ws"), Some(origin), Some(&token))
                .await
                .unwrap_err(),
            403,
            "{origin}"
        );
        assert_eq!(
            socket(&ws(bravo, "/api/ping"), Some(origin), None)
                .await
                .unwrap_err(),
            403,
            "{origin}"
        );
    }
    // How far away: who answers, with no token; and nothing else.
    let hello = socket(&ws(bravo, "/api/ping"), Some(&fleet.https_origin(0)), None)
        .await
        .unwrap();
    assert_eq!(hello["station"], json!("bravo"));
    let mut said: Vec<&String> = hello.as_object().unwrap().keys().collect();
    said.sort();
    assert_eq!(said, ["build", "caps", "station", "version"]);
    // Everything else refuses another origin, a member's deck included.
    for path in ["/api/rpc", "/api/device/request", "/api/peer/rpc"] {
        let resp = client
            .post(bravo.url(path))
            .header("origin", fleet.http_origin(0))
            .bearer_auth(&token)
            .json(&json!({"method": "station.info", "params": {}, "name": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{path}");
    }
    let font = client
        .get(bravo.url("/api/font/x"))
        .header("origin", fleet.http_origin(0))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(font.status(), 403);
    let file = client
        .get(bravo.url("/app.js"))
        .header("origin", fleet.http_origin(0))
        .send()
        .await
        .unwrap();
    assert!(file.headers().get("access-control-allow-origin").is_none());
    // A station on 0.4.10 (here: the switch off) refuses the other origin.
    assert_eq!(
        socket(
            &ws(zulu, "/api/ws"),
            Some(&fleet.http_origin(0)),
            Some(&token)
        )
        .await
        .unwrap_err(),
        403
    );
    assert_eq!(
        socket(&ws(zulu, "/api/ping"), Some(&fleet.http_origin(0)), None)
            .await
            .unwrap_err(),
        403
    );

    // --- Renewal: a new certificate is taken, said, and logged.
    let before = block(&|| alpha.call("station.info", json!({})))["https"]["expires_ms"]
        .as_u64()
        .unwrap();
    make_cert(&fleet.https[0].1, 90);
    block(&|| alpha.call("https.refresh", json!({})));
    let after = tokio::task::block_in_place(|| {
        wait_for("the renewed certificate", 20, || {
            let now = alpha.call("station.info", json!({}))["https"]["expires_ms"]
                .as_u64()
                .unwrap();
            (now > before).then_some(now)
        })
    });
    assert!(after - before > 29 * 24 * 3_600_000);
    let logged = block(&|| json!(alpha.log("https.certificate")));
    assert!(
        logged
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["data"]["renewed"] == json!(true)),
        "{logged}"
    );
    assert!(!logged.to_string().contains("PRIVATE"));

    // --- A station that is no longer a member is no deck's origin.
    tokio::task::block_in_place(|| {
        let mut fleet = fleet;
        fleet.stations[2].kill9();
        let (bravo, zulu_origin) = (&fleet.stations[1], fleet.http_origin(2));
        bravo.call("fleet.forget", json!({"station": "zulu"}));
        let rt = tokio::runtime::Handle::current();
        let refused = rt.block_on(socket(
            &format!("ws://127.0.0.1:{}/api/ws", bravo.port),
            Some(&zulu_origin),
            Some(&token),
        ));
        assert_eq!(refused.unwrap_err(), 403);
    });
}

/// Reason: DECISIONS.md, "One tap, where an administrator's right is
/// needed" (the owner, 17 Nov 2026: "just approve from the menu"): exactly
/// one command, the password on its standard input only and kept nowhere,
/// the owner's call alone and never passed between stations, and each way
/// it can fail said apart. A stand-in for `sudo` and one for `tailscale`
/// are the system's programs here (debug builds only).
#[cfg(unix)]
#[test]
fn turning_https_on_runs_one_command_and_keeps_no_password() {
    use std::os::unix::fs::PermissionsExt;
    let system = tempfile::tempdir().unwrap();
    let seen = system.path().join("seen");
    let script = |name: &str, body: String| {
        let at = system.path().join(name);
        std::fs::write(&at, body).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // sudo: its arguments are written down; what it reads decides.
    script(
        "sudo",
        format!(
            r#"#!/bin/sh
echo "sudo:$*" >> {seen}
read -r pw
case "$pw" in
  right-pw-*) shift 4; exec "$@" ;;
  second) echo "Verification code: " >&2; echo "sudo: 1 incorrect password attempt" >&2; exit 1 ;;
  outsider) echo "$USER is not in the sudoers file." >&2; exit 1 ;;
  *) echo "Sorry, try again." >&2; echo "sudo: no password was provided" >&2; echo "sudo: 1 incorrect password attempt" >&2; exit 1 ;;
esac
"#,
            seen = seen.display()
        ),
    );
    script(
        "tailscale",
        format!("#!/bin/sh\necho \"tailscale:$*\" >> {}\n", seen.display()),
    );
    let dir = system.path().to_str().unwrap().to_string();
    let mut both = Station::fleet(&["alpha", "bravo"], |_, _| {
        (
            String::new(),
            vec![("SPANDECK_TEST_SYSTEM_DIR".to_string(), dir.clone())],
        )
    });
    let (b, a) = (both.pop().unwrap(), both.pop().unwrap());
    a.wait_ready();
    b.wait_ready();
    let user = spandeck_terminal::sys::user_name().unwrap();
    let code = |r: Result<Value, spandeck_core::client::ClientError>| match r {
        Err(e) => (e.code, e.message),
        other => panic!("{other:?}"),
    };
    let ask = |password: &str| a.try_call("tailnet.operator", json!({"password": password}));

    // Not the owner at this station: refused, and nothing is run.
    let pane = a.fake(json!([]), json!({}));
    let (c, _) = code(a.call_as_pane(
        &pane,
        "tailnet.operator",
        json!({"password": "right-pw-agent"}),
    ));
    assert_eq!(c, "denied");
    wait_for("the stations seeing each other", 20, || {
        b.sees("alpha").filter(|s| s == "online")
    });
    let (c, m) = code(b.try_call("peer.call", json!({"station": "alpha", "method": "tailnet.operator", "params": {"password": "right-pw-peer"}})));
    assert_eq!(c, "denied", "{m}");
    assert!(m.contains("cannot be forwarded"), "{m}");
    assert!(!seen.exists(), "nothing was run");

    // What sudo can say, each told apart; then the right password.
    let (c, m) = code(ask("second"));
    assert_eq!(c, "other_prompt", "{m}");
    assert!(
        m.contains("Verification code:") && m.contains("sudo tailscale set --operator="),
        "{m}"
    );
    let (c, m) = code(ask("outsider"));
    assert_eq!(c, "not_allowed", "{m}");
    let (c, m) = code(ask("a guess"));
    assert_eq!(c, "wrong_password", "{m}");
    assert!(m.contains(&format!("{user}'s password on alpha")), "{m}");
    assert_eq!(code(a.try_call("tailnet.operator", json!({}))).0, "invalid");
    let done = ask("right-pw-4711").unwrap();
    assert_eq!((&done["done"], &done["user"]), (&json!(true), &json!(user)));
    // Exactly the one command, as an argument vector.
    let ran = std::fs::read_to_string(&seen).unwrap();
    let tailscale = system.path().join("tailscale");
    let line = format!(
        "sudo:-S -k -p  {} set --operator={user}\n",
        tailscale.display()
    );
    assert_eq!(ran.matches(&line).count(), 4, "{ran}");
    assert_eq!(
        ran.replace(&line, ""),
        format!("tailscale:set --operator={user}\n"),
        "{ran}"
    );
    // Taking it back is the same command with no user.
    ask_off(&a, "right-pw-4711");
    assert!(std::fs::read_to_string(&seen)
        .unwrap()
        .ends_with("tailscale:set --operator=\n"));
    // Three wrong tries a minute, then a wait, whatever is typed.
    assert_eq!(code(ask("a guess")).0, "wrong_password");
    assert_eq!(code(ask("right-pw-4711")).0, "too_many");

    // Every use is in the ship's log, and the password is nowhere: not in
    // the log, the database, the daemon's own log or any file it keeps.
    let log = a.log("tailnet.operator");
    let outcomes: Vec<&str> = log
        .iter()
        .map(|e| e["data"]["outcome"].as_str().unwrap())
        .collect();
    for o in [
        "other_prompt",
        "not_allowed",
        "wrong_password",
        "done",
        "too_many",
    ] {
        assert!(outcomes.contains(&o), "{o}: {outcomes:?}");
    }
    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                files(&p, out);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    let mut all = vec![];
    files(&a.root, &mut all);
    files(&b.root, &mut all);
    assert!(all.iter().any(|p| p.ends_with("spandeck.db")));
    for p in all {
        let bytes = std::fs::read(&p).unwrap_or_default();
        for secret in [&b"right-pw-"[..], b"a guess", b"outsider"] {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret),
                "{} holds a password",
                p.display()
            );
        }
    }
}

#[cfg(unix)]
fn ask_off(s: &Station, password: &str) {
    let done = s.call(
        "tailnet.operator",
        json!({"password": password, "off": true}),
    );
    assert_eq!(done["off"], json!(true));
}
