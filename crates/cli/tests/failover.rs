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
///
/// It is made as a strict verifier wants one (macOS's refused the first
/// kind, a bare self-signed certificate, for its key usage: 17 Nov 2026):
/// a leaf for a server (`serverAuth`, `digitalSignature`), named in its
/// subject alternative names as `localhost` and 127.0.0.1, good for well
/// under 398 days, signed by an authority that is one (`CA:TRUE`,
/// `keyCertSign`) and is what a client is told to trust. The station is
/// given the leaf and then the authority, as Tailscale gives a chain.
/// Returns the authority, for a client to trust.
fn make_cert(file: &Path, days: i64) -> String {
    use rcgen::{DnType, ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose};
    assert!(days < 398);
    let (ca_params, ca_key, ca_pem) = authority();
    let issuer = rcgen::Issuer::from_params(ca_params, ca_key);
    let ca_pem = ca_pem.as_str();
    let names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let mut params = rcgen::CertificateParams::new(names).unwrap();
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(days);
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let key = key_pair();
    let cert = params.signed_by(key, &issuer).unwrap();
    let printed = format!("{}{ca_pem}{}", cert.pem(), key.serialize_pem());
    std::fs::write(file, printed).unwrap();
    ca_pem.to_string()
}

/// The authority every test certificate is signed by: made once.
fn authority() -> &'static (rcgen::CertificateParams, rcgen::KeyPair, String) {
    use rcgen::{BasicConstraints, DnType, IsCa, KeyUsagePurpose};
    static CA: std::sync::OnceLock<(rcgen::CertificateParams, rcgen::KeyPair, String)> =
        std::sync::OnceLock::new();
    CA.get_or_init(|| {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(397);
        params
            .distinguished_name
            .push(DnType::CommonName, "spandeck test authority");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        (params, key, pem)
    })
}

/// One key for every certificate a test makes, so that a browser can be
/// told which key to take a certificate from (`spki`).
fn key_pair() -> &'static rcgen::KeyPair {
    static KEY: std::sync::OnceLock<rcgen::KeyPair> = std::sync::OnceLock::new();
    KEY.get_or_init(|| rcgen::KeyPair::generate().unwrap())
}

/// That key as Chrome names one (`--ignore-certificate-errors-spki-list`):
/// a certificate with it is taken as good, and every other is judged as
/// ever. (With every certificate error ignored instead, Chrome's service
/// worker never registers.)
fn spki() -> String {
    use base64::Engine;
    use sha2::Digest;
    let hash = sha2::Sha256::digest(rcgen::PublicKeyData::subject_public_key_info(key_pair()));
    base64::engine::general_purpose::STANDARD.encode(hash)
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
    // When it runs out is the owner's to read, not an agent's.
    assert!(https.get("expires_ms").is_none() && https.get("days_left").is_none());
    let status = block(&|| alpha.call("https.status", json!({})));
    assert!(
        (58..=60).contains(&status["days_left"].as_i64().unwrap()),
        "{status}"
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
    let before = block(&|| alpha.call("https.status", json!({})))["expires_ms"]
        .as_u64()
        .unwrap();
    make_cert(&fleet.https[0].1, 90);
    block(&|| alpha.call("https.refresh", json!({})));
    let after = tokio::task::block_in_place(|| {
        wait_for("the renewed certificate", 20, || {
            let now = alpha.call("https.status", json!({}))["expires_ms"]
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

// ------------------------------------------------------------ in a browser

use common::browser::Page;

const API: &str = "document.getElementById('app').api";
#[cfg(unix)]
const FO: &str = "document.getElementById('app').failover";

fn token_for(s: &Station) -> String {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(device_token(s))
}

fn shell(s: &Station, name: &str) -> String {
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn picture(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    if let Some(out) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    }
}

/// Reason: DECISIONS.md, "The list of stations on the device, and choosing
/// one": the list is pruned when a station leaves the fleet; the choice is
/// by latency among the stations that are current enough, a tie going to
/// the one that is always on; and nothing is taken while the owner writes.
/// The rules are plain functions of the page; they run in a browser
/// because the page's code does (the every-change tier has no JavaScript
/// engine).
#[test]
#[ignore = "nightly: a real browser; about three seconds"]
fn the_rules_of_the_list_and_the_choice() {
    let s = Station::start("alpha");
    let page = Page::open(&s, 900, 700);
    let got = page.js(
        r#"import('./roster.js').then((R) => {
        const out = {};
        const c = (station, ms, more = {}) => ({ station, ms, version: '0.4.11', caps: ['deck_via'], alwaysOn: false, url: 'ws://x', ...more });
        const names = (r) => ({ pick: r.pick?.station || null, behind: r.behind, answered: r.answered.map(x => x.station), silent: r.silent.map(x => x.station) });
        // By latency; what did not answer, or cannot be used, is not offered.
        out.latency = names(R.choose([c('m4', 12), c('llm', 31), c('rtx', null), c('old', 3, { cannot: 'needs an update', url: undefined })]));
        // Within 10 ms the one that is always on is preferred; beyond, the faster.
        out.tie = names(R.choose([c('m4', 12), c('llm', 19, { alwaysOn: true })]));
        out.noTie = names(R.choose([c('m4', 12), c('llm', 31, { alwaysOn: true })]));
        // A station on an older version is passed over while a current one answers...
        out.version = names(R.choose([c('behind', 2, { version: '0.4.10' }), c('m4', 40)]));
        // ...and used, and said to be behind, when nothing else does.
        out.onlyBehind = names(R.choose([c('behind', 2, { caps: [] }), c('m4', null)]));
        // The station that stopped answering is never the one chosen.
        out.except = names(R.choose([c('spark2', 1), c('m4', 12)], { except: 'spark2' }));
        out.nobody = names(R.choose([c('m4', null)]));
        // The one being counted down to stays, while it answers and is current.
        out.kept1 = names(R.choose([c('m4', 12), c('llm', 14)], { keep: 'llm' })).pick;
        out.kept2 = names(R.choose([c('m4', 12), c('llm', null)], { keep: 'llm' })).pick;
        out.kept3 = names(R.choose([c('m4', 12), c('llm', 2, { version: '0.4.10' })], { keep: 'llm' })).pick;
        out.versions = [R.olderThan('0.4.9', '0.4.10'), R.olderThan('0.4.10', '0.4.9'), R.olderThan('0.4.10', '0.4.10')];
        // The countdown stands still while the owner writes, or said stop.
        out.count = [R.countdown(5000, 250, { writing: false }), R.countdown(5000, 250, { writing: true }), R.countdown(5000, 250, { stopped: true }), R.countdown(100, 250, {})];
        out.writing = [R.writing(10000, 8000), R.writing(10000, 6000), R.writing(10000, 0)];
        out.words = [R.rowWords(c('m4', 12.4)), R.rowWords(c('llm', 31, { alwaysOn: true })), R.rowWords(c('rtx', null)), R.rowWords({ cannot: 'needs an update' })];
        // The list: members only, and one that left is gone in the same step.
        const st = (station, more = {}) => ({ station, state: 'online', addr: `100.64.0.${station.length}:7391`, version: '0.4.11', caps: ['deck_via'], https: { on: true, name: `${station}.tail0.ts.net`, port: 7392 }, ...more });
        const now = 1e12;
        const seen = R.fromFleet(null, [st('rtx', { https: { on: false } })], now - 5000);
        let r = R.fromFleet(seen, [st('spark2', { self: true, web: ['127.0.0.1:7391', '100.64.0.1:7391'], labels: ['always-on'] }), st('m4'), st('rtx', { state: 'unreachable', https: undefined, caps: undefined }), st('guest', { state: 'not_linked' })], now, { home: 'spark2' });
        out.kept = r.stations.map(s => [s.station, s.http, s.https && `${s.https.name}:${s.https.port}`, s.seen === now]);
        out.home = r.home;
        const later = R.fromFleet(r, [st('spark2', { self: true, web: ['100.64.0.1:7391'] }), st('rtx', { state: 'unreachable' })], now + 1000);
        out.pruned = later.stations.map(s => s.station);
        out.homeKept = later.home;
        // A name or an address that is not one is not kept.
        out.bad = R.fromFleet(null, [st('x', { addr: 'evil.example/x?', https: { on: true, name: 'a b', port: 7392 } })], now).stations.map(s => [s.http, s.https]);
        // Always on: its own label, or on at 97% of at least 24 looks.
        out.always = [R.alwaysOn({ label: true }), R.alwaysOn({ looks: 24, on: 24 }), R.alwaysOn({ looks: 24, on: 22 }), R.alwaysOn({ looks: 5, on: 5 })];
        // A look is counted once in ten minutes.
        let h = R.fromFleet(null, [st('m4')], now);
        h = R.fromFleet(h, [st('m4')], now + 1000);
        h = R.fromFleet(h, [st('m4', { state: 'unreachable' })], now + R.LOOK_EVERY);
        out.looks = [h.stations[0].looks, h.stations[0].on];
        // How each may be reached: an HTTPS page only over wss.
        const cand = (secure) => R.candidates(r, { secure, host: secure ? 'spark2.tail0.ts.net:7392' : '100.64.0.1:7391', home: 'spark2' }).map(x => [x.station, x.url || x.cannot]);
        out.plain = cand(false);
        out.secure = cand(true);
        r.stations.find(s => s.station === 'm4').https = null;
        r.stations.find(s => s.station === 'rtx').caps = [];
        out.secureWithout = R.candidates(r, { secure: true, host: 'h', home: 'spark2' }).map(x => [x.station, x.cannot || 'ok']);
        return out;
    })"#,
    );
    let names = |pick: Value, behind: bool, answered: Value, silent: Value| json!({"pick": pick, "behind": behind, "answered": answered, "silent": silent});
    assert_eq!(
        got["latency"],
        names(
            json!("m4"),
            false,
            json!(["m4", "llm"]),
            json!(["old", "rtx"])
        ),
        "{got}"
    );
    assert_eq!(got["tie"]["pick"], json!("llm"), "{got}");
    assert_eq!(got["noTie"]["pick"], json!("m4"));
    assert_eq!(
        got["version"],
        names(json!("m4"), false, json!(["m4", "behind"]), json!([]))
    );
    assert_eq!(
        got["onlyBehind"],
        names(json!("behind"), true, json!(["behind"]), json!(["m4"]))
    );
    assert_eq!(
        got["except"],
        names(json!("m4"), false, json!(["m4"]), json!([]))
    );
    assert_eq!(
        got["nobody"],
        names(Value::Null, false, json!([]), json!(["m4"]))
    );
    assert_eq!(got["versions"], json!([true, false, false]));
    assert_eq!(
        (&got["kept1"], &got["kept2"], &got["kept3"]),
        (&json!("llm"), &json!("m4"), &json!("m4"))
    );
    assert_eq!(got["count"], json!([4750, 5000, 5000, 0]));
    assert_eq!(got["writing"], json!([true, false, false]));
    assert_eq!(
        got["words"],
        json!([
            "12 ms",
            "31 ms · always on",
            "not answering",
            "needs an update"
        ])
    );
    assert_eq!(
        got["kept"],
        json!([
            ["m4", "100.64.0.2:7391", "m4.tail0.ts.net:7392", true],
            ["rtx", "100.64.0.3:7391", null, false],
            [
                "spark2",
                "100.64.0.1:7391",
                "spark2.tail0.ts.net:7392",
                true
            ]
        ]),
        "{got}"
    );
    assert_eq!(
        (&got["home"], &got["homeKept"]),
        (&json!("spark2"), &json!("spark2"))
    );
    assert_eq!(got["pruned"], json!(["rtx", "spark2"]));
    assert_eq!(got["bad"], json!([[null, null]]));
    assert_eq!(got["always"], json!([true, true, false, false]));
    assert_eq!(got["looks"], json!([2, 1]));
    assert_eq!(
        got["plain"],
        json!([
            ["m4", "ws://100.64.0.2:7391"],
            ["rtx", "ws://100.64.0.3:7391"],
            ["spark2", "ws://100.64.0.1:7391"]
        ])
    );
    assert_eq!(
        got["secure"],
        json!([
            ["m4", "wss://m4.tail0.ts.net:7392"],
            ["rtx", "no secure address"],
            ["spark2", "wss://spark2.tail0.ts.net:7392"]
        ])
    );
    assert_eq!(
        got["secureWithout"],
        json!([
            ["m4", "no secure address"],
            ["rtx", "needs an update"],
            ["spark2", "ok"]
        ])
    );
}

/// What the bar's rows say, top to bottom.
fn rows(page: &Page) -> Vec<(String, String, bool)> {
    let got = page.js("[...document.querySelectorAll('.fo-bar .fo-row')].map(r => [r.dataset.station, r.textContent, r.classList.contains('off')])");
    got.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap().to_string(),
                r[1].as_str().unwrap().to_string(),
                r[2].as_bool().unwrap(),
            )
        })
        .collect()
}

/// The scenario of the owner's question (17 Nov 2026): "If spark2 goes
/// down while I am using the mobile app, does it switch over to another
/// spandeck automatically?" Four stations: the one the page is loaded
/// from, two more that serve both HTTP and HTTPS, and one that answers as
/// a station on 0.4.10. At a phone's size, once over HTTP and once over
/// HTTPS: the serving station is killed; the bar names it and lists the
/// stations that answer with their latency, the old one as needing an
/// update; nothing is taken while the owner writes; then the fastest is
/// taken, the pane on screen is attached again through it, the draft is
/// still in its box, and when the first station is back "Go back" is
/// offered and works. About fifty seconds.
#[test]
#[ignore = "nightly: a real browser and four stations, twice; about fifty seconds"]
fn a_page_carries_on_through_the_fastest_station_that_answers() {
    for secure in [false, true] {
        let mut fleet = Fleet::start(&[
            ("alpha", "https"),
            ("bravo", "https"),
            ("charlie", "https"),
            ("zulu", "old"),
        ]);
        fleet.settle();
        let token = token_for(&fleet.stations[0]);
        let pane = shell(&fleet.stations[1], "work");
        let origin = if secure {
            fleet.https_origin(0)
        } else {
            fleet.http_origin(0)
        };
        let page = Page::open_as_device(
            &format!("{origin}/#/s/bravo/p/{pane}"),
            &token,
            1100,
            900,
            &spki(),
        );
        page.phone_screen();
        let which = if secure { "https" } else { "http" };
        // The list is on the device, with how each station can be reached.
        page.wait(
            "the list of stations",
            "JSON.parse(localStorage.getItem('spandeck.roster') || '{}').stations?.length === 4",
        );
        let roster = page.js("JSON.parse(localStorage.getItem('spandeck.roster'))");
        assert_eq!(roster["home"], json!("alpha"), "{roster}");
        let bravo = roster["stations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["station"] == json!("bravo"))
            .unwrap()
            .clone();
        assert_eq!(
            bravo["http"],
            json!(format!("127.0.0.1:{}", fleet.stations[1].port))
        );
        assert_eq!(
            bravo["https"],
            json!({"name": "localhost", "port": fleet.https[1].0})
        );
        // The pane of another station, on screen, with something half written.
        page.wait(
            "the pane's box",
            "!!document.querySelector('.composer-box')",
        );
        page.js("(b => { b.value = 'half a thought'; b.dispatchEvent(new Event('input', { bubbles: true })); window.__page = 'the same page'; return true; })(document.querySelector('.composer-box'))");

        fleet.stations[0].kill9();
        // Within twelve seconds the bar names the station and offers the others.
        page.wait("the bar", "document.querySelector('.fo-bar .link-said')?.textContent === 'alpha is not answering' && document.querySelectorAll('.fo-bar .fo-row').length >= 3");
        let listed = rows(&page);
        let answered: Vec<&(String, String, bool)> = listed.iter().filter(|r| !r.2).collect();
        assert_eq!(answered.len(), 2, "{which}: {listed:?}");
        let ms = |r: &(String, String, bool)| -> f64 {
            let words = r.1.split(" · ").nth(1).unwrap();
            words
                .strip_suffix(" ms")
                .unwrap_or_else(|| panic!("{words}"))
                .parse()
                .unwrap()
        };
        assert!(
            answered
                .iter()
                .all(|r| ["bravo", "charlie"].contains(&r.0.as_str())),
            "{listed:?}"
        );
        // Fastest first, unless they are within the 10 ms that count as the same.
        assert!(ms(answered[0]) <= ms(answered[1]) + 10.0, "{listed:?}");
        // The station on 0.4.10 is listed, greyed, with why; never offered.
        let old = listed.iter().find(|r| r.0 == "zulu").expect("zulu");
        assert!(old.2 && old.1.ends_with("needs an update"), "{old:?}");
        assert!(!listed.iter().any(|r| r.0 == "alpha"), "{listed:?}");
        // While the owner writes, nothing is taken, and the draft is said to be kept.
        for _ in 0..12 {
            page.js("(b => { b.dispatchEvent(new KeyboardEvent('keydown', { key: 'x', bubbles: true })); return true; })(document.querySelector('.composer-box'))");
            std::thread::sleep(Duration::from_millis(500));
        }
        let waiting = page.js("document.querySelector('.fo-count').textContent");
        assert!(
            waiting
                .as_str()
                .unwrap()
                .starts_with("Waiting while you write, then continuing on "),
            "{waiting}"
        );
        assert!(
            waiting.as_str().unwrap().contains("Your draft is kept."),
            "{waiting}"
        );
        assert_eq!(
            page.js(&format!("{API}.target")),
            Value::Null,
            "nothing was taken meanwhile"
        );
        // Then the countdown, which the owner can read.
        page.wait("the countdown", "/^Continuing on (bravo|charlie) in [1-5] s\\./.test(document.querySelector('.fo-count')?.textContent || '')");
        let pick = page
            .js("document.querySelector('.fo-count').textContent.split(' ')[2]")
            .as_str()
            .unwrap()
            .to_string();
        picture(&page, &format!("{which}-1-bar-picker-countdown"));
        // The fastest is taken by itself.
        page.wait("the page working through another station", &format!("!!{API}.target && {API}.connected && ['bravo', 'charlie'].includes({API}.station) && /This deck is now working through/.test(document.querySelector('.via-bar')?.textContent || '')"));
        let through = page
            .js(&format!("{API}.station"))
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(through, pick, "the one the bar counted down to");
        let via = page.js("document.querySelector('.via-bar').textContent");
        assert!(
            via.as_str()
                .unwrap()
                .contains(&format!("This deck is now working through {through}.")),
            "{via}"
        );
        assert!(
            via.as_str().unwrap().contains("Your draft is kept."),
            "{via}"
        );
        picture(&page, &format!("{which}-2-now-working-through"));
        // The same page, never loaded again: the draft is in its box.
        assert_eq!(
            page.js("[window.__page, document.querySelector('.composer-box')?.value]"),
            json!(["the same page", "half a thought"])
        );
        // The pane is attached again: what it prints now reaches the page.
        fleet.stations[1].call(
            "pane.send_text",
            json!({"pane": pane, "text": "echo after-$((40+3))\n"}),
        );
        page.wait("the pane through the other station", "[...document.querySelectorAll('.xterm-rows > div')].map(r => r.textContent).join('\\n').includes('after-43')");
        // The station it works through says so in its log.
        let i = fleet
            .stations
            .iter()
            .position(|s| s.name == through)
            .unwrap();
        let logged = fleet.stations[i].log("deck.via");
        assert!(
            logged.iter().any(|e| e["data"]["from"] == json!("alpha")),
            "{logged:?}"
        );
        // Back: offered, never taken by itself.
        fleet.stations[0].launch();
        fleet.stations[0].wait_ready();
        page.wait(
            "Go back",
            "document.querySelector('.via-bar .fo-back')?.textContent === 'Go back to alpha'",
        );
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(
            page.js(&format!("{API}.station")),
            json!(through),
            "not back by itself"
        );
        picture(&page, &format!("{which}-3-go-back"));
        page.js("(document.querySelector('.via-bar .fo-back').click(), true)");
        page.wait("the page back on its own station", &format!("!{API}.target && {API}.connected && {API}.station === 'alpha' && !document.querySelector('.via-bar')"));
        assert_eq!(
            page.js("[window.__page, document.querySelector('.composer-box')?.value]"),
            json!(["the same page", "half a thought"])
        );
    }
}

/// Reason: DECISIONS.md, "The app saved on the device" and "On opening".
/// Over HTTPS the deck is kept by its worker: exactly the build's files and
/// nothing of the API. With its own station down the page still opens,
/// and works through another without a word asked; with every station
/// down it says so, lists them, and comes back by itself. A new build
/// replaces the saved one whole, when the owner says. And the switch that
/// takes the saved copy away does. About fifty seconds.
#[test]
#[ignore = "nightly: a real browser and a service worker; about fifty seconds"]
fn the_saved_deck_opens_with_its_station_down_and_takes_a_new_build_whole() {
    let mut fleet = Fleet::start(&[("alpha", "https"), ("bravo", "https")]);
    fleet.settle();
    let token = token_for(&fleet.stations[0]);
    let page = Page::open_as_device(
        &format!("{}/", fleet.https_origin(0)),
        &token,
        1100,
        900,
        &spki(),
    );
    page.phone_screen();
    // Saved: one cache, named for the build, holding the build's files.
    page.wait(
        "the worker in charge",
        "!!navigator.serviceWorker.controller",
    );
    let build = page
        .js(&format!("{API}.build"))
        .as_str()
        .unwrap()
        .to_string();
    let kept = page.js("caches.keys().then(async (keys) => [keys, (await (await caches.open(keys[0])).keys()).map(r => new URL(r.url).pathname).sort()])");
    assert_eq!(
        kept[0],
        json!([format!("spandeck-shell-{build}")]),
        "{kept}"
    );
    let files: Vec<&str> = kept[1]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    for f in [
        "/index.html",
        "/app.js",
        "/api.js",
        "/failover.js",
        "/roster.js",
        "/app.css",
        "/vendor/xterm.js",
        "/manifest.webmanifest",
        "/icon-192.png",
    ] {
        assert!(files.contains(&f), "{f} is not kept: {files:?}");
    }
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("/api/") || *f == "/sw.js"),
        "{files:?}"
    );
    // Nothing that carries the token is in it.
    let leaked = page.js(&format!(
        "caches.open('spandeck-shell-{build}').then(async (c) => {{ for (const r of await c.keys()) {{ if ((await (await c.match(r)).text()).includes({})) return r.url; }} return null; }})",
        json!(token)
    ));
    assert_eq!(leaked, Value::Null);
    page.wait(
        "the list of stations",
        "JSON.parse(localStorage.getItem('spandeck.roster') || '{}').stations?.length === 2",
    );
    // An installed app: the manifest and the phone's own lines.
    let app = page.js("fetch('manifest.webmanifest').then(r => r.json()).then(m => [m.display, m.icons.map(i => i.sizes), document.querySelector('link[rel=apple-touch-icon]').getAttribute('href'), document.querySelector('meta[name=apple-mobile-web-app-capable]').content])");
    assert_eq!(
        app,
        json!([
            "standalone",
            ["192x192", "512x512", "any"],
            "icon-180.png",
            "yes"
        ])
    );

    // Its own station down: the page opens from the saved copy and works
    // through the other one, with nothing asked.
    fleet.stations[0].kill9();
    page.reload();
    page.wait("the saved deck working through bravo", &format!("!!document.querySelector('.topbar') && {API}?.connected && {API}.station === 'bravo' && /working through bravo/.test(document.querySelector('.via-bar')?.textContent || '')"));
    assert_eq!(page.js("location.origin"), json!(fleet.https_origin(0)));
    // Every station down: it says so, and lists them with when they were seen.
    fleet.stations[1].kill9();
    page.reload();
    page.wait("the screen that says no station answers", "!!document.querySelector('.fo-none') && document.querySelectorAll('.fo-none-row').length === 2");
    let said = page
        .js("document.querySelector('.fo-none').innerText")
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        said.contains("No station of your fleet is answering."),
        "{said}"
    );
    assert!(
        said.contains("Is Tailscale connected on this device?"),
        "{said}"
    );
    assert!(
        said.contains("alpha") && said.contains("bravo") && said.contains("last seen"),
        "{said}"
    );
    picture(&page, "https-4-no-station");
    // One comes back: so does the page, by itself.
    fleet.stations[1].launch();
    page.wait(
        "the deck again, through bravo",
        &format!(
            "!!document.querySelector('.topbar') && {API}?.connected && {API}.station === 'bravo'"
        ),
    );

    // A new build at its own station. With something half written the page
    // is not loaded again under the owner's hands: a bar offers it.
    fleet.stations[0]
        .env
        .push(("SPANDECK_TEST_BUILD".into(), "the next build".into()));
    fleet.stations[0].launch();
    fleet.stations[0].wait_ready();
    page.wait("Go back", "!!document.querySelector('.via-bar .fo-back')");
    page.js("(() => { const b = document.createElement('textarea'); b.id = 'draft'; b.value = 'half a thought'; document.body.append(b); window.__page = 'the old build'; return true; })()");
    page.js("(document.querySelector('.via-bar .fo-back').click(), true)");
    page.wait(
        "the bar that offers the new build",
        "!!document.querySelector('.shell-bar')",
    );
    assert_eq!(page.js("[window.__page, document.querySelector('script[src^=\"app.js\"]').getAttribute('src')]"), json!(["the old build", format!("app.js?v={build}")]));
    // The new shell is whole in a cache of its own before it is offered.
    let both = page.js("caches.keys().then(async (keys) => Promise.all(keys.sort().map(async k => (await (await caches.open(k)).keys()).length)))");
    assert_eq!(
        both,
        json!([files.len(), files.len()]),
        "two whole shells while one waits"
    );
    picture(&page, "https-7-new-version-ready");
    page.js("(document.querySelector('.shell-bar button').click(), true)");
    page.wait("the page of the new build", &format!("!window.__page && !!document.querySelector('.topbar') && {API}?.connected && {API}.build !== {}", json!(build)));
    let next = page
        .js(&format!("{API}.build"))
        .as_str()
        .unwrap()
        .to_string();
    let now = page.js("caches.keys().then(k => [k, document.querySelector('script[src^=\"app.js\"]').getAttribute('src')])");
    assert_eq!(
        now,
        json!([
            [format!("spandeck-shell-{next}")],
            format!("app.js?v={next}")
        ]),
        "one build, whole"
    );

    // Taken away: the station's switch serves a worker that deletes the
    // copy and unregisters itself.
    fleet.stations[0].call("station.flags", json!({"set": {"app_shell": false}}));
    page.js("navigator.serviceWorker.getRegistration().then(r => r.update()).then(() => true)");
    page.wait("the saved copy gone", "Promise.all([caches.keys(), navigator.serviceWorker.getRegistrations()]).then(([k, r]) => k.length === 0 && r.length === 0)");
}

/// Reason: the owner, 17 Nov 2026: "I don't want to have to run a shell
/// command, just approve from the menu". On a station that Tailscale gives
/// no certificate (as Linux without the operator set), the deck offers to
/// turn secure access on; the dialog says what it does, takes the
/// machine's password, says so when it is wrong, and after the right one
/// the station serves HTTPS and the dialog says where. At a phone's size.
/// About ten seconds.
#[cfg(unix)]
#[test]
#[ignore = "nightly: a real browser; about ten seconds"]
fn secure_access_is_turned_on_from_the_deck() {
    use std::os::unix::fs::PermissionsExt;
    let system = tempfile::tempdir().unwrap();
    let allowed = system.path().join("operator-set");
    let cert = system.path().join("cert-and-key.pem");
    make_cert(&cert, 90);
    let script = |name: &str, body: String| {
        let at = system.path().join(name);
        std::fs::write(&at, body).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        at
    };
    script("sudo", "#!/bin/sh\nread -r pw\nif [ \"$pw\" = \"the right one\" ]; then shift 4; exec \"$@\"; fi\necho 'Sorry, try again.' >&2\necho 'sudo: 1 incorrect password attempt' >&2\nexit 1\n".to_string());
    script(
        "tailscale",
        format!("#!/bin/sh\necho \"$*\" > {}\n", allowed.display()),
    );
    // `tailscale cert`, as it answers a user who is not root or the operator.
    let certs = script(
        "cert",
        format!(
            "#!/bin/sh\nif [ -f {} ]; then cat {}; else echo 'Access denied: cert access denied' >&2; exit 1; fi\n",
            allowed.display(),
            cert.display()
        ),
    );
    let port = free_port();
    let s = Station::start_with(
        "alpha",
        &format!(
            "[web]\nhttps_port = {port}\nname = \"localhost\"\ncert_cmd = ['{}']\n",
            certs.display()
        ),
        &[("SPANDECK_TEST_SYSTEM_DIR", system.path().to_str().unwrap())],
    );
    wait_for("the station finding it may not ask", 20, || {
        (s.call("station.info", json!({}))["https"]["state"] == json!("needs_operator"))
            .then_some(())
    });
    let user = spandeck_terminal::sys::user_name().unwrap();
    let page = Page::open(&s, 1100, 900);
    page.phone_screen();
    // The station's menu offers it.
    page.wait(
        "the station on the page",
        &format!("{API}.connected && !!{FO}"),
    );
    let offered = page.js(&format!(
        "{API}.call('fleet.view', {{}}).then(v => v.stations[0].https.state)"
    ));
    assert_eq!(offered, json!("needs_operator"));
    page.js(&format!(
        "{API}.call('fleet.view', {{}}).then(v => ({FO}.httpsDialog(v.stations[0]), true))"
    ));
    page.wait(
        "the dialog",
        "!!document.querySelector('.https-dialog input[type=password]')",
    );
    let said = page
        .js("document.querySelector('.dialog').innerText")
        .as_str()
        .unwrap()
        .to_string();
    assert!(said.contains("Secure access (HTTPS) on alpha"), "{said}");
    assert!(
        said.contains(&format!(
            "spandeck asks alpha to let your user ({user}) manage Tailscale there"
        )),
        "{said}"
    );
    assert!(
        said.contains(
            "This needs your password on alpha once; it is used for that one command and not kept."
        ),
        "{said}"
    );
    assert!(said.contains("your agents included, can change Tailscale's settings on that machine without a password"), "{said}");
    picture(&page, "https-5-turn-on-secure-access");
    // The dialog fits a phone's screen.
    let fits = page.js("(r => r.left >= 0 && r.right <= innerWidth && r.bottom <= innerHeight)(document.querySelector('.dialog').getBoundingClientRect())");
    assert_eq!(fits, json!(true));
    let give = |password: &str| {
        page.js(&format!("(i => {{ i.value = {}; return true; }})(document.querySelector('.https-dialog input[type=password]'))", json!(password)));
        page.js("([...document.querySelectorAll('.https-dialog button')].find(b => b.textContent === 'Turn on').click(), true)");
    };
    give("a guess");
    page.wait("the wrong password said", &format!("(document.querySelector('.https-said')?.textContent || '').includes(\"That is not {user}'s password on alpha\")"));
    assert!(!allowed.exists());
    // The box is empty again, and nothing of the password is on the page.
    assert_eq!(page.js("[document.querySelector('.https-dialog input[type=password]').value, document.body.innerHTML.includes('a guess')]"), json!(["", false]));
    give("the right one");
    page.wait("the dialog saying it is on", &format!("(document.querySelector('.https-dialog')?.innerText || '').includes('The deck is served at https://localhost:{port}')"));
    assert_eq!(
        std::fs::read_to_string(&allowed).unwrap().trim(),
        format!("set --operator={user}")
    );
    picture(&page, "https-6-secure-access-on");
    let https = s.call("station.info", json!({}))["https"].clone();
    assert_eq!(
        (&https["on"], &https["state"]),
        (&json!(true), &json!("on")),
        "{https}"
    );
}
