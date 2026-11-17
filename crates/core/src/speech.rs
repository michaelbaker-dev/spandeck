//! Speech to text on the owner's own machine (DECISIONS.md, "The
//! microphone", 17 Nov 2026). Off unless `[speech] command` is set.
//!
//! A deck records a short clip and hands it to a station that says it can
//! (`caps`: `speech`). The station writes the clip to a file only it can
//! read, runs the owner's configured program on it, answers with what the
//! program printed, and deletes the file: at once, whatever happened.
//! spandeck ships no model and calls no service; where the sound goes is
//! wherever that program sends it, which for whisper.cpp or the like is
//! nowhere.
//!
//! The owner's alone (`rpc.rs`): an agent, another station acting for
//! itself and a read-only caller are refused. The text is returned to the
//! deck, which puts it in the message box for the owner to read and send;
//! nothing here types into a pane.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Result};
use crate::paths::SpeechConfig;

/// Said in `caps` by a station whose owner configured a command.
pub const CAP: &str = "speech";
/// The most text taken from the program.
const MAX_TEXT: usize = 20_000;

/// One clip at a time: a model fills the machine it runs on.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Turn;
impl Drop for Turn {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// The clip on disk, for as long as this lives and no longer.
struct Clip(PathBuf);
impl Drop for Clip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The file ending the program is given, by what the browser said it
/// recorded. Nothing else is taken.
pub fn ending(mime: &str) -> Option<&'static str> {
    let kind = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match kind.as_str() {
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" | "video/mp4" => "m4a",
        "audio/webm" | "video/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mpeg" => "mp3",
        _ => return None,
    })
}

/// The command with the clip's path in it: where `{file}` stands, or last.
pub fn argv(command: &[String], file: &Path) -> Vec<String> {
    let path = file.display().to_string();
    let mut out: Vec<String> = command.iter().map(|a| a.replace("{file}", &path)).collect();
    if !command.iter().any(|a| a.contains("{file}")) {
        out.push(path);
    }
    out
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(bytes)
}

impl SpeechConfig {
    /// The owner's command, when there is one.
    pub fn configured(&self) -> Option<&[String]> {
        self.command
            .as_deref()
            .filter(|c| !c.is_empty() && !c[0].trim().is_empty())
    }
}

impl Daemon {
    /// `speech.transcribe`: `{data_b64, mime, seconds?}` → `{text, ms}`.
    pub async fn speech_transcribe(&self, params: &Value) -> Result<Value> {
        let cfg = &self.cfg.speech;
        let Some(command) = cfg.configured() else {
            return Err(invalid(format!(
                "{} has no speech command: set [speech] command in its config.toml",
                self.station
            )));
        };
        let mime = params["mime"].as_str().unwrap_or("");
        let Some(ext) = ending(mime) else {
            return Err(invalid(format!(
                "a recording of this kind is not taken: {}",
                mime.chars().take(60).collect::<String>()
            )));
        };
        // What the deck says the clip lasts. The station cannot hear it
        // without the model, so the size and the time the program is given
        // are the limits that hold; this one refuses an honest page early.
        if let Some(secs) = params["seconds"].as_f64() {
            if secs > cfg.max_seconds as f64 + 1.0 {
                return Err(invalid(format!(
                    "the recording is longer than {} s",
                    cfg.max_seconds
                )));
            }
        }
        let b64 = params["data_b64"]
            .as_str()
            .ok_or_else(|| invalid("data_b64 is missing"))?;
        // Before it is decoded: four letters carry three bytes.
        if b64.len() / 4 * 3 > cfg.max_bytes + 3 {
            return Err(invalid(format!(
                "the recording is larger than {} KB",
                cfg.max_bytes / 1024
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| invalid("data_b64 is not base64"))?;
        if bytes.is_empty() {
            return Err(invalid("the recording is empty"));
        }
        if BUSY.swap(true, Ordering::SeqCst) {
            return Err(invalid(format!(
                "{} is transcribing another recording: try again in a moment",
                self.station
            )));
        }
        let _turn = Turn;
        let dir = self.paths.speech_dir();
        std::fs::create_dir_all(&dir)?;
        let clip = Clip(dir.join(format!(
            "{}.{ext}",
            ulid::Ulid::generate().to_string().to_lowercase()
        )));
        write_private(&clip.0, &bytes)?;
        let argv = argv(command, &clip.0);
        let started = Instant::now();
        let limit = Duration::from_secs(cfg.timeout_secs.max(1));
        let child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Given up on: the program is ended with the wait for it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| invalid(format!("the speech command did not start: {e}")))?;
        let out = match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(out) => out?,
            Err(_) => {
                self.speech_said(bytes.len(), started, None, "timeout");
                return Err(invalid(format!(
                    "the speech command gave no answer within {} s and was ended",
                    limit.as_secs()
                )));
            }
        };
        drop(clip);
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            let why: String = why
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            self.speech_said(bytes.len(), started, None, "failed");
            return Err(invalid(format!(
                "the speech command failed ({}){}{}",
                out.status,
                if why.is_empty() { "" } else { ": " },
                why
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let text: String = text.trim().chars().take(MAX_TEXT).collect();
        self.speech_said(bytes.len(), started, Some(text.chars().count()), "ok");
        Ok(
            json!({"text": text, "ms": started.elapsed().as_millis() as u64, "station": self.station}),
        )
    }

    /// The ship's log says that a clip was transcribed, never what was said.
    fn speech_said(&self, bytes: usize, started: Instant, chars: Option<usize>, how: &str) {
        self.record(
            "speech.transcribed",
            None,
            None,
            None,
            None,
            json!({"bytes": bytes, "ms": started.elapsed().as_millis() as u64, "chars": chars, "result": how}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The program is run as written, with no shell: the clip's path
    /// stands where `{file}` is, or last.
    #[test]
    fn the_clip_is_named_where_the_command_says_or_last() {
        let f = Path::new("/s/speech/a.m4a");
        assert_eq!(
            argv(&["w".into(), "-f".into(), "{file}".into(), "-nt".into()], f),
            ["w", "-f", "/s/speech/a.m4a", "-nt"]
        );
        assert_eq!(argv(&["w".into()], f), ["w", "/s/speech/a.m4a"]);
    }

    /// Only a sound recording's name becomes a file ending.
    #[test]
    fn only_a_recording_is_taken() {
        assert_eq!(ending("audio/mp4"), Some("m4a"));
        assert_eq!(ending("audio/webm;codecs=opus"), Some("webm"));
        assert_eq!(ending("text/x-shellscript"), None);
        assert_eq!(ending("../../x"), None);
        assert_eq!(ending(""), None);
    }
}
