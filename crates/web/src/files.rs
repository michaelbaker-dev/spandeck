//! A file of a station, sent to the owner's browser (the owner's request
//! of 19 Nov 2026; DECISIONS.md, "The files of a station").
//!
//! `GET /api/files/get?station=<name>&path=<absolute path>` (or `&zip=<id>`,
//! and `&inline=1`), for the owner's signed-in browser: the token in a
//! header, as for a font, or `?ticket=<t>` from `files.ticket`, since a
//! browser that saves a download asks for it with no header of ours.
//!
//! The file is read a piece at a time (`files.read`) and each piece is sent
//! on before the next is read: a file of many gigabytes is never held in
//! memory, here or on the station it comes from. For another station the
//! pieces are asked through the forward that exists, one call each.
//!
//! What a file holds is never trusted. It goes out as an attachment and as
//! bytes of no type, with `nosniff` and a policy that lets it run nothing:
//! a file an agent wrote must never be a page of the deck. The one
//! exception is a preview (`inline=1`) of a file whose first bytes say PNG,
//! JPEG, GIF or WebP, which goes out as that image. SVG and HTML are
//! attachments, always.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Value};
use spandeck_core::daemon::{Authority, Caller, Daemon};
use spandeck_core::fleet::Remote;
use spandeck_core::server::error_json;

use super::{caller, client_id, identify, origin_ok, refuse, Page, Via, Web, NO_TAILSCALE};

/// A piece read from this station, and the first and the largest read
/// from another (whose calls keep the limits every call between stations
/// has; a piece that fails is asked again, smaller).
const PIECE_HERE: u64 = 1 << 20;
const PIECE_FAR_START: u64 = 256 * 1024;
const PIECE_FAR_MAX: u64 = 1 << 20;
const PIECE_MIN: u64 = 64 * 1024;
/// How long a zip that is still being built is waited for.
const ZIP_WAIT: Duration = Duration::from_secs(120);

/// PNG, JPEG, GIF or WebP, by the first bytes. Nothing else is ever shown
/// in the page.
pub(crate) fn image_type(first: &[u8]) -> Option<&'static str> {
    if first.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if first.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if first.starts_with(b"GIF87a") || first.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if first.len() >= 12 && &first[..4] == b"RIFF" && &first[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `Content-Disposition`, with the name twice: plainly for a browser that
/// reads only that, and in full (RFC 5987) for every other.
pub(crate) fn disposition(inline: bool, name: &str) -> String {
    let plain: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' | '/' | ';' | '%' => '_',
            c if c == ' ' || c.is_ascii_graphic() => c,
            _ => '_',
        })
        .collect();
    let full: String = name
        .bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() || b"-._~".contains(&b) => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect();
    let how = if inline { "inline" } else { "attachment" };
    format!("{how}; filename=\"{plain}\"; filename*=UTF-8''{full}")
}

fn failed(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message.into()}})),
    )
        .into_response()
}

fn refused(e: &spandeck_core::daemon::Error) -> Response {
    let status = match e.code() {
        "denied" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid" => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({"error": error_json(e)}))).into_response()
}

/// What is read, from where, and how far it has got.
struct Feed {
    daemon: Daemon,
    who: Caller,
    /// Another station, or none for this one.
    station: Option<String>,
    /// `{"path": …}` or `{"zip": …}`.
    what: Value,
    at: u64,
    size: u64,
    piece: u64,
    first: Option<Vec<u8>>,
}

impl Feed {
    async fn ask(&self, method: &str, params: Value) -> spandeck_core::daemon::Result<Value> {
        match &self.station {
            Some(station) => {
                self.daemon
                    .dispatch(
                        &self.who,
                        "peer.call",
                        json!({"station": station, "method": method, "params": params}),
                    )
                    .await
            }
            None => self.daemon.dispatch(&self.who, method, params).await,
        }
    }

    /// The next piece, from where the last one ended.
    async fn piece(&self) -> Result<Vec<u8>, String> {
        let mut params = self.what.clone();
        params["offset"] = json!(self.at);
        params["len"] = json!(self.piece.min(self.size.saturating_sub(self.at)).max(1));
        let got = self
            .ask("files.read", params)
            .await
            .map_err(|e| e.to_string())?;
        base64::engine::general_purpose::STANDARD
            .decode(got["data_b64"].as_str().unwrap_or(""))
            .map_err(|e| format!("a bad piece: {e}"))
    }

    /// A piece, asked up to three times, each time smaller.
    async fn next(&mut self) -> Result<Vec<u8>, String> {
        let mut tries = 0;
        loop {
            let began = std::time::Instant::now();
            match self.piece().await {
                Ok(bytes) => {
                    if self.station.is_some() && began.elapsed() < Duration::from_secs(1) {
                        self.piece = (self.piece * 2).min(PIECE_FAR_MAX);
                    }
                    return Ok(bytes);
                }
                Err(why) => {
                    tries += 1;
                    if tries >= 3 {
                        return Err(why);
                    }
                    self.piece = (self.piece / 2).max(PIECE_MIN);
                    tokio::time::sleep(Duration::from_millis(300 * tries)).await;
                }
            }
        }
    }
}

pub(crate) async fn get(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Via(secure): Via,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Err(e) = origin_ok(&web, &headers, secure, false) {
        return refuse(e);
    }
    // Who asks, and for what. A ticket says both: it was given to the
    // owner a minute ago at the most, for one file, by this station.
    let (who, asked) = match q.get("ticket") {
        Some(ticket) => {
            match identify(&web, addr.ip()).await {
                Remote::Loopback | Remote::Owner { .. } => {}
                Remote::Stranger(_) => {
                    return refuse((
                        StatusCode::FORBIDDEN,
                        "this device is not under your Tailscale login",
                    ))
                }
                Remote::NoTailscale => {
                    return refuse((StatusCode::SERVICE_UNAVAILABLE, NO_TAILSCALE))
                }
            }
            let Some(asked) = web.daemon.files_ticket(ticket) else {
                return refuse((
                    StatusCode::UNAUTHORIZED,
                    "this download's link is no longer good: ask for the download again",
                ));
            };
            let who = Caller {
                authority: Authority::Owner,
                pane: None,
                client: client_id(&headers, &addr),
                who: "you (download)".into(),
            };
            (who, asked)
        }
        None => match caller(&web, &addr, &headers, None, &Page::Own).await {
            Ok(who) if who.is_owner() => {
                let asked = json!({
                    "station": q.get("station"),
                    "path": q.get("path"),
                    "zip": q.get("zip"),
                    "inline": q.get("inline").is_some_and(|v| v == "1"),
                });
                (who, asked)
            }
            Ok(_) => {
                return refuse((
                    StatusCode::FORBIDDEN,
                    "a station's files are for the user's clients",
                ))
            }
            Err(e) => return refuse(e),
        },
    };
    let station = asked["station"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != web.daemon.station)
        .map(String::from);
    let (path, zip) = (asked["path"].as_str(), asked["zip"].as_str());
    let mut feed = Feed {
        daemon: web.daemon.clone(),
        who,
        piece: match station {
            Some(_) => PIECE_FAR_START,
            None => PIECE_HERE,
        },
        station,
        what: json!({}),
        at: 0,
        size: 0,
        first: None,
    };
    let name = match (path, zip) {
        (Some(path), None) => {
            let stat = match feed.ask("files.stat", json!({"path": path})).await {
                Ok(stat) => stat,
                Err(e) => return refused(&e),
            };
            let is_file = stat["kind"] == json!("file")
                || (stat["kind"] == json!("link") && stat["target"] == json!("file"));
            if !is_file {
                return failed(
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    format!("{path} is not a file: a folder is downloaded as a zip"),
                );
            }
            feed.what = json!({"path": path});
            feed.size = stat["size"].as_u64().unwrap_or(0);
            stat["name"].as_str().unwrap_or("file").to_string()
        }
        (None, Some(id)) => {
            let began = std::time::Instant::now();
            let status = loop {
                let status = match feed.ask("files.zip.status", json!({"id": id})).await {
                    Ok(status) => status,
                    Err(e) => return refused(&e),
                };
                match status["state"].as_str() {
                    Some("ready") => break status,
                    Some("building") if began.elapsed() < ZIP_WAIT => {
                        tokio::time::sleep(Duration::from_millis(400)).await
                    }
                    Some("building") => return failed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "not_ready",
                        "the zip is still being made: ask again when files.zip.status says ready",
                    ),
                    _ => {
                        return failed(
                            StatusCode::CONFLICT,
                            status["code"].as_str().unwrap_or("failed"),
                            status["message"].as_str().unwrap_or("the zip was not made"),
                        )
                    }
                }
            };
            feed.what = json!({"zip": id});
            feed.size = status["size"].as_u64().unwrap_or(0);
            status["name"].as_str().unwrap_or("files.zip").to_string()
        }
        _ => {
            return failed(
                StatusCode::BAD_REQUEST,
                "invalid",
                "say one of path and zip",
            )
        }
    };
    // The first piece before any header is sent: what cannot be read is an
    // error with a status, not a download that ends early; and a preview
    // is decided by these bytes.
    let first = match feed.size {
        0 => vec![],
        _ => match feed.next().await {
            Ok(bytes) => bytes,
            Err(why) => return failed(StatusCode::BAD_GATEWAY, "internal", why),
        },
    };
    let inline = asked["inline"] == json!(true);
    let image = image_type(&first).filter(|_| inline && zip.is_none());
    feed.first = Some(first);
    let size = feed.size;
    let body = futures_util::stream::unfold(feed, |mut feed| async move {
        let ended = |why: String| std::io::Error::other(why);
        if feed.at >= feed.size {
            return None;
        }
        let bytes = match feed.first.take() {
            Some(first) => Ok(first),
            None => feed.next().await,
        };
        let left = (feed.size - feed.at) as usize;
        match bytes {
            // The file is shorter than it was: the length promised cannot
            // be kept, and the browser is told the download broke.
            Ok(bytes) if bytes.is_empty() => {
                feed.at = feed.size;
                Some((
                    Err(ended("the file became shorter while it was read".into())),
                    feed,
                ))
            }
            Ok(mut bytes) => {
                bytes.truncate(left);
                feed.at += bytes.len() as u64;
                Some((Ok(Bytes::from(bytes)), feed))
            }
            Err(why) => {
                feed.at = feed.size;
                Some((Err(ended(why)), feed))
            }
        }
    });
    let mut resp = Response::new(Body::from_stream(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(image.unwrap_or("application/octet-stream")),
    );
    if let Ok(v) = HeaderValue::from_str(&disposition(image.is_some(), &name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rule 4 of the files decision: only a file whose first bytes say PNG,
    /// JPEG, GIF or WebP is ever shown in the page; SVG and HTML, which can
    /// run script, never are, whatever their names.
    #[test]
    fn only_four_kinds_of_image_are_ever_shown_in_the_page() {
        assert_eq!(
            image_type(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some("image/png")
        );
        assert_eq!(image_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a..."), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        for not in [
            &b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"[..],
            b"<!doctype html><script>alert(1)</script>",
            b"<?xml version=\"1.0\"?><svg/>",
            b"RIFF\x10\0\0\0WAVEfmt ",
            b"%PDF-1.7",
            b"",
        ] {
            assert_eq!(image_type(not), None);
        }
    }

    /// A name goes into a header: nothing in it ends the header's value or
    /// starts another, and a name that is not ASCII is given in full.
    #[test]
    fn a_files_name_cannot_break_out_of_its_header() {
        assert_eq!(
            disposition(false, "report.pdf"),
            "attachment; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
        let odd = disposition(false, "a\"b\r\nSet-Cookie: x;é.txt");
        assert_eq!(
            odd,
            "attachment; filename=\"a_b__Set-Cookie: x__.txt\"; filename*=UTF-8''a%22b%0D%0ASet-Cookie%3A%20x%3B%C3%A9.txt"
        );
        assert!(HeaderValue::from_str(&odd).is_ok());
        assert!(disposition(true, "x.png").starts_with("inline; "));
    }
}
