//! The files of a station (the owner's request of 19 Nov 2026; DECISIONS.md,
//! "The files of a station"), with real stations: whose the methods are,
//! what a download goes out as, a copy from one station to another, and
//! "Move agent here".
mod common;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use common::*;
use serde_json::{json, Value};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";
const PAGE: &[u8] = b"<!doctype html><script>document.title = 'ran'</script>";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Upload a file in pieces of `piece` bytes, to this station or through it
/// to another, as the page does.
fn upload(
    s: &Station,
    to: Option<&str>,
    dir: &Path,
    name: &str,
    bytes: &[u8],
    piece: usize,
) -> Value {
    let id = format!("test-upload-{name}-0123456789")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
    let mut at = 0;
    loop {
        let end = (at + piece).min(bytes.len());
        let params = json!({
            "id": id, "dir": dir, "name": name, "offset": at,
            "data_b64": b64(&bytes[at..end]), "done": end == bytes.len(),
        });
        let out = match to {
            Some(station) => s.call(
                "peer.call",
                json!({"station": station, "method": "files.put", "params": params}),
            ),
            None => s.call("files.put", params),
        };
        assert_eq!(out["ok"], json!(true), "{out}");
        if end == bytes.len() {
            return out;
        }
        at = end;
    }
}

struct Got {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

impl Got {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }
}

fn get(rt: &tokio::runtime::Runtime, s: &Station, query: &str, token: Option<&str>) -> Got {
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let mut req = client.get(s.url(&format!("/api/files/get?{query}")));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.unwrap();
        Got {
            status: resp.status().as_u16(),
            headers: resp.headers().clone(),
            body: resp.bytes().await.unwrap().to_vec(),
        }
    })
}

fn enc(path: &Path) -> String {
    path.display()
        .to_string()
        .bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// A call signed with the fleet's key, as another station makes one.
fn as_a_station(rt: &tokio::runtime::Runtime, s: &Station, method: &str, params: Value) -> Value {
    let body = json!({"method": method, "params": params, "from": "bravo", "protocol": 2})
        .to_string()
        .into_bytes();
    let sig = spandeck_core::trust::sign_with(&[0x5a; 32], "rpc", &body);
    rt.block_on(async {
        reqwest::Client::new()
            .post(s.url("/api/peer/rpc"))
            .header("x-spandeck-station", "bravo")
            .header("x-spandeck-sig", sig)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    })
}

/// Rule 1 of the decision: every `files.` method is the owner's alone. An
/// agent with its pane's token is refused, and so is another station
/// calling for itself; a station only carries the owner's own request.
/// And rule 4: what is downloaded is an attachment of no type that can run
/// nothing; an HTML file is never shown in the page, whatever is asked;
/// only a real image is, and only when a preview is asked for.
#[test]
fn a_stations_files_are_the_owners_and_a_download_never_runs_in_the_page() {
    let s = Station::keyed_on("alpha", free_port(), "");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = s.root.join("home/docs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret.env"), b"KEY=abc").unwrap();
    let secret = dir.join("secret.env");

    // ---- an agent
    let pane = s.fake(json!({"steps": []}), json!({}));
    for (method, params) in [
        ("files.home", json!({})),
        ("files.list", json!({"path": dir})),
        ("files.stat", json!({"path": secret})),
        ("files.read", json!({"path": secret})),
        (
            "files.put",
            json!({"id": "agent-upload-0123456789", "dir": dir, "name": "x", "data_b64": "", "done": true}),
        ),
        ("files.mkdir", json!({"dir": dir, "name": "x"})),
        ("files.create", json!({"dir": dir, "name": "x"})),
        ("files.rename", json!({"path": secret, "name": "x"})),
        ("files.trash", json!({"paths": [secret]})),
        ("files.restore", json!({"trash": "tr-0123456789abcdef"})),
        ("files.delete", json!({"paths": [secret]})),
        (
            "files.transfer",
            json!({"op": "copy", "paths": [secret], "dir": dir}),
        ),
        ("files.jobs", json!({})),
        ("files.job.cancel", json!({"id": "x"})),
        ("files.zip", json!({"paths": [secret]})),
        ("files.zip.status", json!({"id": "x"})),
        ("files.ticket", json!({"path": secret})),
        ("pane.restart", json!({"pane": pane, "cwd": dir})),
    ] {
        let refused = s.call_as_pane(&pane, method, params.clone()).unwrap_err();
        assert_eq!(
            refused.code, "denied",
            "an agent's {method}: {}",
            refused.message
        );
        // ---- another station, for itself
        let peer = as_a_station(&rt, &s, method, params);
        assert_eq!(
            peer["error"]["code"],
            json!("denied"),
            "a station's {method}: {peer}"
        );
    }
    assert_eq!(std::fs::read(&secret).unwrap(), b"KEY=abc");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    // The same station carrying the owner's request is answered.
    let carried = as_a_station(
        &rt,
        &s,
        "rpc.forward",
        json!({"method": "files.stat", "params": {"path": secret}}),
    );
    assert_eq!(carried["result"]["size"], json!(7), "{carried}");
    // Except a download's ticket, which is the station's the browser asks.
    let ticket = as_a_station(
        &rt,
        &s,
        "rpc.forward",
        json!({"method": "files.ticket", "params": {"path": secret}}),
    );
    assert_eq!(ticket["error"]["code"], json!("denied"), "{ticket}");

    // ---- the owner: an upload in pieces, and what the log says of it
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let done = upload(&s, None, &dir, "data.bin", &big, 70_000);
    assert_eq!(done["path"], json!(dir.join("data.bin")));
    assert_eq!(std::fs::read(dir.join("data.bin")).unwrap(), big);
    upload(&s, None, &dir, "page.html", PAGE, 1 << 20);
    upload(&s, None, &dir, "dot.png", PNG, 1 << 20);
    upload(
        &s,
        None,
        &dir,
        "drawing.svg",
        b"<svg xmlns='http://www.w3.org/2000/svg'><script>1</script></svg>",
        1 << 20,
    );
    let logged = s.log("files.uploaded");
    assert_eq!(logged.len(), 4);
    let line = logged
        .iter()
        .find(|l| l.to_string().contains("page.html"))
        .unwrap();
    assert!(line.to_string().contains("\"bytes\""), "{line}");
    assert!(!line.to_string().contains("doctype") && !line.to_string().contains(&b64(PAGE)));

    // ---- the download
    let token = s.token();
    let file = get(
        &rt,
        &s,
        &format!("path={}", enc(&dir.join("data.bin"))),
        Some(&token),
    );
    assert_eq!(file.status, 200);
    assert_eq!(file.body, big);
    assert_eq!(file.header("content-length"), "300000");
    assert_eq!(file.header("content-type"), "application/octet-stream");
    assert_eq!(
        file.header("content-disposition"),
        "attachment; filename=\"data.bin\"; filename*=UTF-8''data.bin"
    );
    assert_eq!(file.header("x-content-type-options"), "nosniff");
    assert_eq!(
        file.header("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(file.header("cross-origin-resource-policy"), "same-origin");
    // HTML and SVG: attachments of no type, with or without "inline".
    for name in ["page.html", "drawing.svg"] {
        for inline in ["", "&inline=1"] {
            let got = get(
                &rt,
                &s,
                &format!("path={}{inline}", enc(&dir.join(name))),
                Some(&token),
            );
            assert_eq!(got.status, 200);
            assert_eq!(
                got.header("content-type"),
                "application/octet-stream",
                "{name}{inline}"
            );
            assert!(
                got.header("content-disposition")
                    .starts_with("attachment; "),
                "{name}{inline}"
            );
            assert_eq!(got.header("x-content-type-options"), "nosniff");
        }
    }
    // A real image: shown only when a preview is asked for.
    let png = enc(&dir.join("dot.png"));
    let plain = get(&rt, &s, &format!("path={png}"), Some(&token));
    assert_eq!(plain.header("content-type"), "application/octet-stream");
    assert!(plain
        .header("content-disposition")
        .starts_with("attachment; "));
    let shown = get(&rt, &s, &format!("path={png}&inline=1"), Some(&token));
    assert_eq!(shown.header("content-type"), "image/png");
    assert!(shown.header("content-disposition").starts_with("inline; "));
    assert_eq!(
        shown.header("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(shown.body, PNG);
    // An HTML file named like an image is still what its bytes say.
    std::fs::write(dir.join("fake.png"), PAGE).unwrap();
    let fake = get(
        &rt,
        &s,
        &format!("path={}&inline=1", enc(&dir.join("fake.png"))),
        Some(&token),
    );
    assert_eq!(fake.header("content-type"), "application/octet-stream");
    assert!(fake
        .header("content-disposition")
        .starts_with("attachment; "));

    // ---- who may download
    let path = format!("path={}", enc(&secret));
    assert_eq!(get(&rt, &s, &path, None).status, 401);
    assert_eq!(get(&rt, &s, &path, Some("wrong")).status, 401);
    assert_eq!(get(&rt, &s, &path, Some(&s.pane_token(&pane))).status, 403);
    let other_origin = rt.block_on(async {
        reqwest::Client::new()
            .get(s.url(&format!("/api/files/get?{path}")))
            .header("origin", "http://evil.example")
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    });
    assert_eq!(other_origin, 403);
    assert_eq!(get(&rt, &s, "path=/no/such/file", Some(&token)).status, 404);
    assert_eq!(
        get(&rt, &s, &format!("path={}", enc(&dir)), Some(&token)).status,
        400
    );
    // A ticket: a link the browser can follow with no header, for that
    // one file, and nothing without it.
    let ticket = s.call("files.ticket", json!({"path": secret}));
    let by_ticket = get(
        &rt,
        &s,
        &format!("ticket={}", ticket["ticket"].as_str().unwrap()),
        None,
    );
    assert_eq!(
        (by_ticket.status, by_ticket.body.as_slice()),
        (200, &b"KEY=abc"[..])
    );
    assert!(ticket["url"]
        .as_str()
        .unwrap()
        .starts_with("/api/files/get?ticket="));
    assert_eq!(get(&rt, &s, "ticket=not-one", None).status, 401);

    // ---- several things as one zip
    let zip = s.call("files.zip", json!({"paths": [dir]}));
    let id = zip["zip"].as_str().unwrap();
    assert_eq!(zip["name"], json!("docs.zip"));
    let zipped = get(&rt, &s, &format!("zip={id}"), Some(&token));
    assert_eq!(zipped.status, 200);
    assert!(zipped.body.starts_with(b"PK\x03\x04"));
    assert_eq!(zipped.header("content-type"), "application/octet-stream");
    assert!(zipped.header("content-disposition").contains("docs.zip"));
    let status = s.call("files.zip.status", json!({"id": id}));
    assert_eq!(status["state"], json!("ready"));
    assert_eq!(status["size"], json!(zipped.body.len()));
    assert_eq!(status["files_done"], json!(6));
}

/// "Move agent here": `pane.restart` with a folder starts the pane's
/// program again there, under the same pane and name; the pane shows the
/// new folder, and it holds after the station is started again. A folder
/// that is not one is refused and the pane is left as it was. A pane of a
/// project moves inside its project (and the project's file then names
/// the folder), never out of it.
#[test]
fn a_pane_is_moved_to_another_folder_and_stays_there() {
    let mut s = Station::start("alpha");
    let home = s.root.join("home");
    let there = home.join("work/deep");
    std::fs::create_dir_all(&there).unwrap();
    std::fs::write(home.join("a-file"), b"x").unwrap();
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": home}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Not a folder: refused, and the shell is still the same shell.
    s.call(
        "pane.send_text",
        json!({"pane": sh, "text": "echo still-$((40+2))"}),
    );
    s.wait_text(&sh, "still-42");
    for bad in [home.join("a-file"), home.join("nowhere"), "relative".into()] {
        let refused = s
            .try_call("pane.restart", json!({"pane": sh, "cwd": bad}))
            .unwrap_err();
        assert_eq!(refused.code, "invalid", "{}", refused.message);
    }
    assert!(s.text(&sh).contains("still-42"));
    assert_eq!(s.pane(&sh)["cwd"], json!(home));
    // Moved.
    let moved = s.call("pane.restart", json!({"pane": sh, "cwd": there}));
    assert_eq!(
        (moved["id"].clone(), moved["name"].clone()),
        (json!(sh), json!("sh"))
    );
    assert_eq!(moved["cwd"], json!(there));
    s.call(
        "pane.send_text",
        json!({"pane": sh, "text": "echo in:$(pwd -P | tr a-z A-Z)"}),
    );
    let real = std::fs::canonicalize(&there).unwrap();
    s.wait_text(
        &sh,
        &format!("in:{}", real.display().to_string().to_uppercase()),
    );
    let logged = s.log("pane.restarted");
    assert_eq!(logged[0]["data"]["cwd"], json!(there), "{logged:?}");
    assert_eq!(logged[0]["data"]["moved_from"], json!(home));

    // An agent: a new conversation in the new folder, and after the
    // station is started again it is in that folder still.
    let agent = s.fake(json!({"steps": []}), json!({"name": "ag"}));
    let first = wait_for("a session id", 10, || {
        s.pane(&agent)["session_id"].as_str().map(String::from)
    });
    s.call("pane.restart", json!({"pane": agent, "cwd": there}));
    let second = wait_for("the new conversation's id", 10, || {
        s.pane(&agent)["session_id"]
            .as_str()
            .filter(|id| *id != first)
            .map(String::from)
    });
    assert!(!s.text(&agent).contains("RESUMED"), "{}", s.text(&agent));

    // A pane of a project: inside the project, and the project's file
    // says where; never out of it.
    let repo = s.git_repo("proj");
    std::fs::create_dir_all(repo.join("crates/core")).unwrap();
    s.call(
        "project.new",
        json!({"path": repo, "name": "proj", "panes": [{"name": "build", "agent": "shell"}], "open": true}),
    );
    let build = wait_for("the project's pane", 10, || {
        s.call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("build"))
            .and_then(|p| p["id"].as_str().map(String::from))
    });
    let out = s
        .try_call("pane.restart", json!({"pane": build, "cwd": there}))
        .unwrap_err();
    assert!(
        out.code == "invalid"
            && out.message.contains("project proj")
            && out.message.contains("inside its project"),
        "{}",
        out.message
    );
    assert_eq!(s.pane(&build)["cwd"], json!(repo));
    let inside = s.call(
        "pane.restart",
        json!({"pane": build, "cwd": repo.join("crates/core")}),
    );
    assert_eq!(inside["cwd"], json!(repo.join("crates/core")));
    let toml = std::fs::read_to_string(repo.join(".spandeck/project.toml")).unwrap();
    assert!(toml.contains("cwd = \"crates/core\""), "{toml}");

    s.kill9();
    s.launch();
    wait_for("the agent back in its folder", 20, || {
        s.try_call("pane.get", json!({"pane": agent}))
            .ok()
            .filter(|p| p["exited"] != json!(true) && p["cwd"] == json!(there))
    });
    s.wait_text(&agent, &format!("RESUMED {second}"));
    assert_eq!(s.pane(&sh)["cwd"], json!(there));
}

/// The owner, 19 Nov 2026: "we should not delete anything that is in use,
/// we would ask the user to close those panes before deleting. If no
/// panes are open then we could delete panes and projects". Decided at the
/// station: the folder a pane it lists was started in, and every folder
/// above it, is not trashed, deleted, renamed or moved; the answer says
/// `in_use` and names the panes; what is free in the same call goes ahead.
/// With the pane closed the folder goes like any other, a project's too:
/// the project then says its folder is gone, and Undo brings it back.
#[test]
fn nothing_a_pane_works_in_is_deleted_renamed_or_moved() {
    let s = Station::start("alpha");
    let home = s.root.join("home");
    let a = home.join("a");
    for dir in ["a/b/deep", "a/bc", "dest"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    std::fs::write(a.join("free.txt"), b"x").unwrap();
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "builder", "cwd": a.join("b")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let codes = |out: &Value| -> Vec<String> {
        out["failed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_string())
            .collect()
    };
    let moved = |paths: Value| {
        let job = s.call(
            "files.transfer",
            json!({"op": "move", "paths": paths, "dir": home.join("dest")}),
        )["job"]
            .clone();
        wait_for("the move to finish", 20, || {
            let jobs = s.call("files.jobs", json!({}));
            let j = jobs["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == job)
                .cloned()?;
            (j["state"] != json!("running")).then_some(j)
        })
    };

    // The pane's folder, the folder above it, and beside them a folder
    // whose name only begins the same.
    let out = s.call(
        "files.trash",
        json!({"paths": [a.join("b"), a.join("bc"), a]}),
    );
    assert_eq!(out["trashed"], json!([a.join("bc")]), "{out}");
    assert_eq!(codes(&out), ["in_use", "in_use"], "{out}");
    let said = &out["failed"][0];
    assert_eq!(said["panes"][0]["name"], json!("builder"), "{out}");
    assert_eq!(said["panes"][0]["id"], json!(sh));
    assert!(
        said["message"]
            .as_str()
            .unwrap()
            .starts_with("builder is working in "),
        "{said}"
    );
    let out = s.call("files.delete", json!({"paths": [a.join("b"), a]}));
    assert_eq!(codes(&out), ["in_use", "in_use"], "{out}");
    for (path, name) in [(a.join("b"), "c"), (a.clone(), "z")] {
        let out = s.call("files.rename", json!({"path": path, "name": name}));
        assert_eq!(
            (out["ok"].clone(), out["code"].clone()),
            (json!(false), json!("in_use")),
            "{out}"
        );
        assert_eq!(out["panes"][0]["name"], json!("builder"));
    }
    let job = moved(json!([a.join("b"), a.join("free.txt")]));
    assert_eq!(job["errors"][0]["code"], json!("in_use"), "{job}");
    assert_eq!(job["errors"][0]["panes"][0]["name"], json!("builder"));
    assert!(home.join("dest/free.txt").exists(), "what is free is moved");
    assert!(a.join("b/deep").is_dir(), "nothing in use was touched");
    assert!(s.pane(&sh)["exited"] != json!(true));

    // Ended and still listed, it is still in the way; closed, it is not.
    s.call("pane.send_text", json!({"pane": sh, "text": "exit"}));
    wait_for("the shell to end", 10, || {
        (s.pane(&sh)["exited"] == json!(true)).then_some(())
    });
    let out = s.call("files.trash", json!({"paths": [a.join("b")]}));
    assert_eq!(out["failed"][0]["panes"][0]["ended"], json!(true), "{out}");
    s.call("pane.close", json!({"pane": sh}));
    let out = s.call("files.rename", json!({"path": a.join("b"), "name": "c"}));
    assert_eq!(out["ok"], json!(true), "{out}");
    let job = moved(json!([a.join("c")]));
    assert_eq!(job["errors"], json!([]), "{job}");
    assert!(home.join("dest/c/deep").is_dir());
    let out = s.call("files.trash", json!({"paths": [a]}));
    assert_eq!(out["trashed"], json!([a]), "{out}");
    let out = s.call("files.delete", json!({"paths": [home.join("dest/c")]}));
    assert_eq!(codes(&out), [] as [&str; 0], "{out}");

    // A project: not while a pane of it is listed; then like any folder.
    let repo = s.git_repo("tally");
    s.call(
        "project.new",
        json!({"path": repo, "name": "tally", "panes": [{"name": "build", "agent": "shell"}], "open": true}),
    );
    let project = || {
        s.call("project.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("tally"))
            .cloned()
            .expect("the project is still listed")
    };
    wait_for("the project's pane", 10, || {
        project()["panes"]
            .as_array()
            .filter(|p| !p.is_empty())
            .map(|_| ())
    });
    let out = s.call("files.trash", json!({"paths": [repo]}));
    assert_eq!(codes(&out), ["in_use"], "{out}");
    assert_eq!(
        out["failed"][0]["panes"][0],
        json!({"id": project()["panes"][0]["id"], "name": "build", "project": "tally", "ended": false})
    );
    assert!(
        out["failed"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("build (project tally) is working in "),
        "{out}"
    );
    s.call("project.close", json!({"name": "tally"}));
    let out = s.call("files.trash", json!({"paths": [repo]}));
    assert_eq!(out["trashed"], json!([repo]), "{out}");
    assert!(!repo.exists());
    // The project is still listed and says what became of its folder;
    // opening it is refused in a sentence, and nothing is started.
    let gone = project();
    let problem = gone["problem"].as_str().unwrap_or_default();
    assert!(
        problem.contains("the folder of project tally")
            && problem.contains("deleted, renamed or moved"),
        "{gone}"
    );
    assert_eq!(gone["panes"], json!([]));
    let refused = s
        .try_call("project.open", json!({"name": "tally"}))
        .unwrap_err();
    assert!(
        refused.code == "not_found" && refused.message.contains("Put it back"),
        "{}: {}",
        refused.code,
        refused.message
    );
    assert!(!repo.exists(), "opening a project does not make its folder");
    // Undo: the folder is back, and the project opens as before.
    let back = s.call("files.restore", json!({"trash": out["trash"]}));
    assert_eq!(back["restored"], json!([repo]), "{back}");
    assert!(project()["problem"].is_null(), "{}", project());
    let opened = s.call("project.open", json!({"name": "tally"}));
    assert_eq!(opened["panes"][0]["cwd"], json!(repo), "{opened}");
}

/// Between stations a copy is pushed by the station that has the files,
/// as the owner's own `files.mkdir` and `files.put` forwarded to the
/// other: a folder tree arrives whole, with its modes, `files.jobs` says
/// how far it is, the conflict rule holds for the top-level item, and a
/// move between stations is refused. And a file of the other station is
/// downloaded through the station the browser is on.
#[test]
#[ignore = "merge tier: two stations"]
fn a_folder_is_copied_from_one_station_to_another_and_downloaded_through_the_first() {
    let (a, b) = Station::pair();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let from = a.root.join("home/project");
    let to = b.root.join("home/inbox");
    std::fs::create_dir_all(from.join("src/deep")).unwrap();
    std::fs::create_dir_all(&to).unwrap();
    let big: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(from.join("src/deep/big.bin"), &big).unwrap();
    std::fs::write(from.join("src/main.rs"), b"fn main() {}").unwrap();
    std::fs::write(from.join("empty"), b"").unwrap();
    std::fs::write(from.join("run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(from.join("run.sh"), std::fs::Permissions::from_mode(0o750))
            .unwrap();
        std::os::unix::fs::symlink(from.join("src"), from.join("src-link")).unwrap();
    }
    let finished = |job: &str| {
        wait_for("the copy to finish", 60, || {
            let jobs = a.call("files.jobs", json!({}));
            let j = jobs["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == json!(job))
                .cloned()?;
            (j["state"] != json!("running")).then_some(j)
        })
    };
    let copy = |conflict: &str| {
        let started = a.call(
            "files.transfer",
            json!({"op": "copy", "paths": [from], "to": "bravo", "dir": to, "conflict": conflict}),
        );
        finished(started["job"].as_str().unwrap())
    };
    let job = copy("fail");
    assert_eq!(job["state"], json!("done"), "{job}");
    assert_eq!(job["errors"], json!([]), "{job}");
    assert_eq!(job["to"], json!("bravo"));
    assert_eq!(
        (job["files_done"].clone(), job["files_total"].clone()),
        (json!(4), json!(4))
    );
    assert_eq!(job["bytes_done"], job["bytes_total"]);
    assert_eq!(job["created"], json!([to.join("project")]));
    assert_eq!(
        std::fs::read(to.join("project/src/deep/big.bin")).unwrap(),
        big
    );
    assert_eq!(
        std::fs::read(to.join("project/src/main.rs")).unwrap(),
        b"fn main() {}"
    );
    assert_eq!(
        std::fs::metadata(to.join("project/empty")).unwrap().len(),
        0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(to.join("project/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
        assert!(!to.join("project/src-link").exists());
        assert_eq!(job["skipped"][0]["code"], json!("link"));
    }
    // No part file is left on the other station, and both logs say what
    // was done, by whom.
    let left: Vec<_> = std::fs::read_dir(to.join("project/src/deep"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(left.len(), 1);
    assert_eq!(a.log("files.copied")[0]["data"]["files"], json!(4));
    assert!(b.log("files.uploaded")[0]["actor"]
        .as_str()
        .unwrap()
        .contains("via alpha"));
    // The name is taken now.
    let again = copy("fail");
    assert_eq!(again["errors"][0]["code"], json!("exists"), "{again}");
    let skipped = copy("skip");
    assert_eq!(
        (
            skipped["errors"].clone(),
            skipped["skipped"][0]["code"].clone()
        ),
        (json!([]), json!("exists"))
    );
    let both = copy("keep_both");
    assert_eq!(both["created"], json!([to.join("project (2)")]), "{both}");
    assert!(to.join("project (2)/src/deep/big.bin").exists());
    std::fs::write(from.join("src/main.rs"), b"fn main() { changed() }").unwrap();
    std::fs::write(to.join("project/only-there"), b"kept").unwrap();
    let merged = copy("replace");
    assert_eq!(merged["errors"], json!([]), "{merged}");
    assert_eq!(
        std::fs::read(to.join("project/src/main.rs")).unwrap(),
        b"fn main() { changed() }"
    );
    assert!(to.join("project/only-there").exists());
    // Between stations: copies, always.
    let moved = a
        .try_call(
            "files.transfer",
            json!({"op": "move", "paths": [from], "to": "bravo", "dir": to}),
        )
        .unwrap_err();
    assert!(moved.message.contains("never moved"), "{}", moved.message);
    assert!(from.join("src/main.rs").exists());

    // The other way round, as the page does it: bravo is asked, through
    // alpha, to copy to alpha.
    let back = a.root.join("home/back");
    std::fs::create_dir_all(&back).unwrap();
    let started = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "files.transfer", "params":
               {"op": "copy", "paths": [to.join("project/src")], "to": "alpha", "dir": back}}),
    );
    wait_for("bravo's copy to alpha", 60, || {
        let jobs = a.call(
            "peer.call",
            json!({"station": "bravo", "method": "files.jobs", "params": {}}),
        );
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == started["job"] && j["state"] == json!("done"))
            .cloned()
    });
    assert_eq!(std::fs::read(back.join("src/deep/big.bin")).unwrap(), big);

    // A file of bravo's, downloaded from alpha's address, in pieces.
    let token = a.token();
    let path = enc(&to.join("project/src/deep/big.bin"));
    let got = get(&rt, &a, &format!("station=bravo&path={path}"), Some(&token));
    assert_eq!(got.status, 200);
    assert_eq!(got.header("content-length"), "5000000");
    assert_eq!(got.header("content-type"), "application/octet-stream");
    assert!(got
        .header("content-disposition")
        .starts_with("attachment; filename=\"big.bin\""));
    assert!(got.body == big, "the bytes that arrived are the file's");
    // An upload through alpha to bravo, and a listing of it from alpha.
    upload(&a, Some("bravo"), &to, "note.txt", b"through alpha", 5);
    assert_eq!(
        std::fs::read(to.join("note.txt")).unwrap(),
        b"through alpha"
    );
    let listed = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "files.list", "params": {"path": to}}),
    );
    let names: Vec<&str> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, ["project", "project (2)", "note.txt"]);
    // And bravo's zip, through alpha.
    let zip = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "files.zip", "params": {"paths": [to.join("project/src")]}}),
    );
    let zipped = get(
        &rt,
        &a,
        &format!("station=bravo&zip={}", zip["zip"].as_str().unwrap()),
        Some(&token),
    );
    assert_eq!(zipped.status, 200);
    assert!(zipped.body.starts_with(b"PK\x03\x04") && zipped.body.len() > big.len());
}
