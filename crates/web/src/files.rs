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
//! JPEG, GIF or WebP, which goes out as that image, or say a film or a
//! sound (MP4, QuickTime, WebM, MP3, M4A, WAV, Ogg), which goes out as that
//! (the owner's request of 21 Nov 2026: an `.mp4` could only be
//! downloaded). SVG and HTML are attachments, always.
//!
//! A part of a file is served when a `Range` asks for one (206): a phone's
//! browser plays no film without it. The type is still decided by the
//! file's first bytes, whichever part is asked for.

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

/// A film or a sound a browser's own player can be given, by the first
/// bytes: MP4 and M4V, QuickTime, M4A, WebM (Matroska), MP3, WAV, Ogg.
/// Never by the name. (A picture kept in the same box as an MP4, HEIC or
/// AVIF, is not one of them.)
pub(crate) fn media_type(first: &[u8]) -> Option<&'static str> {
    if first.len() >= 12 && &first[4..8] == b"ftyp" {
        return match &first[8..12] {
            b"qt  " => Some("video/quicktime"),
            b"M4A " | b"M4B " | b"M4P " => Some("audio/mp4"),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1"
            | b"avif" | b"avis" | b"crx " => None,
            _ => Some("video/mp4"),
        };
    }
    if first.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if first.starts_with(b"ID3") {
        return Some("audio/mpeg");
    }
    if first.len() >= 12 && &first[..4] == b"RIFF" && &first[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    if first.starts_with(b"OggS") {
        return Some("audio/ogg");
    }
    // An MPEG audio frame: eleven bits set, then a version, a layer, a bit
    // rate and a sample rate that exist (so not text that begins FF FE).
    if first.len() >= 4 && first[0] == 0xff && first[1] & 0xe0 == 0xe0 {
        let (version, layer) = ((first[1] >> 3) & 3, (first[1] >> 1) & 3);
        let (rate, sample) = (first[2] >> 4, (first[2] >> 2) & 3);
        if version != 1 && layer != 0 && rate != 0 && rate != 15 && sample != 3 {
            return Some("audio/mpeg");
        }
    }
    None
}

/// What a preview may go out as: one of the four pictures, or a film or a
/// sound. Everything else is an attachment of no type.
pub(crate) fn shown_type(first: &[u8]) -> Option<&'static str> {
    image_type(first).or_else(|| media_type(first))
}

/// The part of a file of `size` bytes a `Range` header asks for: the first
/// and the last byte. `Ok(None)`: no part is asked for in a way this
/// serves (another unit, several parts, not understood), and the whole
/// file is the answer, as RFC 9110 allows. `Err`: it asks for what the
/// file does not have (416).
pub(crate) fn range_of(header: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    let Some((from, to)) = spec.trim().split_once('-') else {
        return Ok(None);
    };
    let (from, to) = (from.trim(), to.trim());
    let number = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    match (from.is_empty(), to.is_empty()) {
        (true, true) => Ok(None),
        // `-n`: the last n bytes.
        (true, false) => match number(to) {
            None => Ok(None),
            Some(0) => Err(()),
            Some(_) if size == 0 => Err(()),
            Some(n) => Ok(Some((size.saturating_sub(n), size - 1))),
        },
        (false, open) => {
            let Some(from) = number(from) else {
                return Ok(None);
            };
            let to = match (open, number(to)) {
                (true, _) => u64::MAX,
                (false, Some(to)) if to >= from => to,
                _ => return Ok(None),
            };
            if from >= size {
                return Err(());
            }
            Ok(Some((from, to.min(size - 1))))
        }
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

    /// The file's first bytes, for a part that begins later: what it is
    /// is said by these, never by where the part begins.
    async fn head(&self) -> Result<Vec<u8>, String> {
        let mut params = self.what.clone();
        params["offset"] = json!(0);
        params["len"] = json!(64);
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
    // owner a minute ago at the most (a preview's, kept good by a player's
    // use of it, four hours ago at the most), for one file, by this station.
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
    // A part of it, when a part is asked for: a film is played from
    // parts, and a phone's browser plays none without them.
    let total = feed.size;
    let part = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        None => None,
        Some(asked) => match range_of(asked, total) {
            Ok(part) => part,
            Err(()) => {
                let mut resp = failed(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "invalid",
                    format!("{name} has {total} bytes: that part of it does not exist"),
                );
                let h = resp.headers_mut();
                h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                if let Ok(v) = HeaderValue::from_str(&format!("bytes */{total}")) {
                    h.insert(header::CONTENT_RANGE, v);
                }
                return resp;
            }
        },
    };
    let (start, end) = part.unwrap_or((0, total.saturating_sub(1)));
    let inline = asked["inline"] == json!(true) && zip.is_none();
    // What it is, by the file's first bytes, also for a part that begins
    // later or is only a few bytes long (a player's first question is
    // often `bytes=0-1`): one small read more.
    let whole_head = start == 0 && (end >= 63 || end + 1 >= total);
    let mut head = match inline && !whole_head {
        true => match feed.head().await {
            Ok(bytes) => bytes,
            Err(why) => return failed(StatusCode::BAD_GATEWAY, "internal", why),
        },
        false => vec![],
    };
    feed.at = start;
    feed.size = match total {
        0 => 0,
        _ => end + 1,
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
    if whole_head {
        head = first[..first.len().min(64)].to_vec();
    }
    let image = shown_type(&head).filter(|_| inline);
    feed.first = Some(first);
    let size = feed.size - start;
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
    if part.is_some() {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    let h = resp.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if part.is_some() {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
            h.insert(header::CONTENT_RANGE, v);
        }
    }
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

    /// The owner's report of 21 Nov 2026: an `.mp4` in the Files panel could
    /// only be downloaded. A film or a sound is known by its first bytes
    /// alone; a page, a drawing and text that only look like one are not.
    #[test]
    fn a_film_or_a_sound_is_known_by_its_first_bytes_only() {
        for (first, is) in [
            (&b"\0\0\0\x18ftypisom\0\0\x02\0"[..], "video/mp4"),
            (b"\0\0\0\x1cftypmp42\0\0\0\0", "video/mp4"),
            (b"\0\0\0\x1cftypM4V \0\0\0\0", "video/mp4"),
            (b"\0\0\0\x14ftypqt  \0\0\0\0", "video/quicktime"),
            (b"\0\0\0\x1cftypM4A \0\0\0\0", "audio/mp4"),
            (b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81", "video/webm"),
            (b"ID3\x04\0\0\0\0\0\x0a", "audio/mpeg"),
            (b"\xff\xfb\x90\x64\0\0", "audio/mpeg"),
            (b"RIFF\x24\0\0\0WAVEfmt ", "audio/wav"),
            (b"OggS\0\x02\0\0", "audio/ogg"),
        ] {
            assert_eq!(media_type(first), Some(is), "{first:?}");
            assert_eq!(shown_type(first), Some(is));
        }
        for not in [
            &b"<!doctype html><script>alert(1)</script>"[..],
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
            // Text in UTF-16 begins FF FE.
            b"\xff\xfe<\0h\0t\0m\0l\0>\0",
            // A picture in an MP4's box is not a film.
            b"\0\0\0\x18ftypheic\0\0\0\0",
            b"\0\0\0\x18ftypavif\0\0\0\0",
            b"RIFF\x10\0\0\0AVI LIST",
            b"%PDF-1.7",
            b"ftyp",
            b"",
        ] {
            assert_eq!(media_type(not), None, "{not:?}");
        }
        assert_eq!(shown_type(b"<html>ftypisom"), None);
        assert_eq!(shown_type(b"GIF89a..."), Some("image/gif"));
    }

    /// The same report: a phone's browser plays a film only from parts of
    /// it. One part is served, as asked; what the file does not have is
    /// refused; and what is not understood gets the whole file.
    #[test]
    fn one_part_of_a_file_is_what_a_range_asks_for() {
        assert_eq!(range_of("bytes=0-1", 1000), Ok(Some((0, 1))));
        assert_eq!(range_of("bytes=0-", 1000), Ok(Some((0, 999))));
        assert_eq!(range_of("bytes=200-299", 1000), Ok(Some((200, 299))));
        assert_eq!(range_of("bytes=999-", 1000), Ok(Some((999, 999))));
        assert_eq!(range_of("bytes=900-5000", 1000), Ok(Some((900, 999))));
        assert_eq!(range_of("bytes=-100", 1000), Ok(Some((900, 999))));
        assert_eq!(range_of("bytes=-5000", 1000), Ok(Some((0, 999))));
        assert_eq!(range_of(" bytes= 5 - 9 ", 1000), Ok(Some((5, 9))));
        // Not there: 416.
        assert_eq!(range_of("bytes=1000-", 1000), Err(()));
        assert_eq!(range_of("bytes=5000-6000", 1000), Err(()));
        assert_eq!(range_of("bytes=-0", 1000), Err(()));
        assert_eq!(range_of("bytes=0-", 0), Err(()));
        assert_eq!(range_of("bytes=-5", 0), Err(()));
        // Not served as a part: the whole file.
        for whole in [
            "bytes=0-1,5-9",
            "lines=0-5",
            "bytes=9-5",
            "bytes=-",
            "bytes=a-b",
            "bytes=+1-5",
            "bytes=5",
            "bytes=0-99999999999999999999",
            "",
        ] {
            assert_eq!(range_of(whole, 1000), Ok(None), "{whole}");
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
