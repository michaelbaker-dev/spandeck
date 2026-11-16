//! A pane's session, found without a hook (DECISIONS.md, "Chat for every
//! pane"; `docs/conversation-api.md`, "How a pane's session is found").
//!
//! Codex runs its hooks only after the owner has reviewed them in Codex,
//! which may be never. Until a hook names the pane's session, the station
//! looks for it among Codex's own session files, and takes one only when it
//! can be no other pane's:
//!
//! - the file is one of Codex's own conversations (not one it started for a
//!   sub-agent), its first line records the pane's working folder, and it
//!   was started at or after the pane's program was;
//! - no other pane is in it (named by that pane's hooks, or by the session
//!   it was started into);
//! - no other pane running Codex in that folder, and reporting nothing
//!   either, had already started when the file was begun: it could be that
//!   pane's, so it is given to neither, and it does not take the place of
//!   a session that is this pane's for certain;
//! - of several that are left, the newest, when every older one had gone
//!   quiet before it began: one program writes one conversation at a time,
//!   and begins another with `/new`. Two written to at the same time are
//!   two programs, and nothing is guessed.
//!
//! A session named by a hook always wins, and takes the place of one found
//! here. What is found is only ever read: answering a question or an
//! approval stays with what the pane's own hook holds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use spandeck_agents::sessions::{self, CodexFile, Folder};
use spandeck_agents::AgentKind;
use spandeck_archive::conversation::ts_ms;

use crate::daemon::{Daemon, PaneMeta};

/// How often the files are looked at again for one pane, while its
/// conversation is asked for: for another session once one is found, and
/// sooner while none is (Codex begins its file with the first message).
const LOOK_EVERY: Duration = Duration::from_secs(2);
const LOOK_SOON: Duration = Duration::from_millis(500);

/// One look for a pane's session: when, for which start of its program,
/// and the session and its file, or why none was taken.
pub(crate) struct Looked {
    at: Instant,
    started_ms: u64,
    found: Result<(String, String), String>,
}

pub(crate) type Finds = HashMap<String, Looked>;

/// A session file that could be a pane's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub started_ms: u64,
    pub modified_ms: u64,
}

/// Which of `files` is the session of a pane whose program started at
/// `pane_started_ms`: its place in `files`, or why none is taken. `files`:
/// the program's own conversations recorded for the pane's folder that no
/// other pane is in. `rivals`: when each other pane began that runs the
/// same program in the same folder and reports nothing either.
pub fn choose(
    files: &[Candidate],
    pane_started_ms: u64,
    rivals: &[u64],
    program: &str,
) -> Result<usize, String> {
    let mine: Vec<usize> = (0..files.len())
        .filter(|i| files[*i].started_ms >= pane_started_ms)
        .collect();
    if mine.is_empty() {
        return Err(format!(
            "no session file of {program}'s for this folder has appeared since the pane started ({program} begins one with its first message)"
        ));
    }
    // A file begun after another such pane had started could be that
    // pane's: it is never taken, and it never takes the place of one that
    // is this pane's for certain.
    let own: Vec<usize> = mine
        .iter()
        .copied()
        .filter(|i| !rivals.iter().any(|r| *r <= files[*i].started_ms))
        .collect();
    if own.is_empty() {
        return Err(format!(
            "another pane runs {program} in this folder and reports nothing either, so a session file begun since both started could be either pane's: spandeck does not guess"
        ));
    }
    // Still written to after every later one began: a conversation that is
    // going on, not one the program left behind with `/new`.
    let live: Vec<usize> = own
        .iter()
        .copied()
        .filter(|i| {
            !own.iter().any(|j| {
                files[*j].started_ms > files[*i].started_ms
                    && files[*i].modified_ms <= files[*j].started_ms
            })
        })
        .collect();
    match live.as_slice() {
        [one] => Ok(*one),
        _ => Err(format!(
            "{} of {program}'s session files for this folder were written to at the same time since the pane started, so more than one {program} is at work here: spandeck does not guess which is this pane's",
            live.len()
        )),
    }
}

impl Daemon {
    /// The session found for a pane the last time it was looked for.
    /// Reads nothing.
    pub(crate) fn found_session(&self, pane: &str) -> Option<String> {
        let finds = self.finds.lock().unwrap();
        finds
            .get(pane)
            .and_then(|l| l.found.as_ref().ok())
            .map(|(id, _)| id.clone())
    }

    /// A hook has named the pane's session, or the pane is gone: what was
    /// found for it counts for nothing.
    pub(crate) fn forget_found(&self, pane: &str) {
        self.finds.lock().unwrap().remove(pane);
    }

    /// The file of a session a pane was started into, for a program whose
    /// files are found by the session's id.
    pub(crate) fn session_file(&self, meta: &PaneMeta, session: &str) -> Option<String> {
        if meta.agent != AgentKind::Codex {
            return None;
        }
        sessions::codex_file_of(&sessions::codex_sessions_dir(&self.home), session)
            .map(|p| p.display().to_string())
    }

    /// The session of a pane whose program has named none, from the
    /// program's own files: `(session, file)`, or why none is taken. `None`
    /// for a program whose files are not looked through. Looked for at most
    /// twice a second for a pane; reads directories and first lines.
    pub(crate) fn find_session(&self, meta: &PaneMeta) -> Option<Result<(String, String), String>> {
        if meta.agent != AgentKind::Codex {
            return None;
        }
        let started = meta.proc_started_ms.unwrap_or(meta.created_ms);
        {
            let finds = self.finds.lock().unwrap();
            if let Some(l) = finds.get(&meta.id) {
                let every = if l.found.is_ok() {
                    LOOK_EVERY
                } else {
                    LOOK_SOON
                };
                if l.started_ms == started && l.at.elapsed() < every {
                    return Some(l.found.clone());
                }
            }
        }
        let found = self.look_for_session(meta, started);
        self.finds.lock().unwrap().insert(
            meta.id.clone(),
            Looked {
                at: Instant::now(),
                started_ms: started,
                found: found.clone(),
            },
        );
        Some(found)
    }

    fn look_for_session(&self, meta: &PaneMeta, started: u64) -> Result<(String, String), String> {
        let program = meta.agent.display();
        let folder = Folder::of(std::path::Path::new(&meta.cwd));
        let mut taken: Vec<String> = vec![];
        let mut rivals: Vec<u64> = vec![];
        for p in self.list_panes() {
            let m = p.meta.lock().unwrap();
            if m.id == meta.id || m.exit.is_some() {
                continue;
            }
            if let Some(id) = &m.session_id {
                taken.push(id.clone());
            }
            if m.agent == meta.agent && !m.hooks_seen && folder.is(&m.cwd) {
                rivals.push(m.proc_started_ms.unwrap_or(m.created_ms));
            }
        }
        let files: Vec<CodexFile> =
            sessions::codex_since(&sessions::codex_sessions_dir(&self.home), started)
                .into_iter()
                .filter(|f| folder.is(&f.cwd) && !taken.contains(&f.id))
                .collect();
        let candidates: Vec<Candidate> = files
            .iter()
            .filter_map(|f| {
                Some(Candidate {
                    id: f.id.clone(),
                    started_ms: ts_ms(&f.started)?,
                    modified_ms: f.modified_ms,
                })
            })
            .collect();
        let i = choose(&candidates, started, &rivals, program)?;
        let file = files
            .iter()
            .find(|f| f.id == candidates[i].id)
            .ok_or_else(|| "its session file is gone".to_string())?;
        Ok((file.id.clone(), file.path.display().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, started_ms: u64, modified_ms: u64) -> Candidate {
        Candidate {
            id: id.into(),
            started_ms,
            modified_ms,
        }
    }

    /// The owner's report of 16 Nov 2026: a Codex pane whose hooks never
    /// ran had no conversation. Its session is found among Codex's files,
    /// and the rule this is held to: never another pane's, and no guess.
    #[test]
    fn a_session_is_taken_only_when_it_can_be_no_other_panes() {
        let pick = |files: &[Candidate], rivals: &[u64]| {
            choose(files, 1000, rivals, "Codex").map(|i| files[i].id.clone())
        };
        // Nothing since the pane started: one from before it is not its.
        let why = pick(&[file("old", 900, 5000)], &[]).unwrap_err();
        assert!(why.starts_with("no session file of Codex's"), "{why}");
        // The one begun since.
        assert_eq!(
            pick(&[file("old", 900, 5000), file("a", 1100, 1200)], &[]).unwrap(),
            "a"
        );
        // `/new`: the first went quiet before the second began.
        let moved = [file("a", 1100, 1900), file("b", 2000, 2500)];
        assert_eq!(pick(&moved, &[]).unwrap(), "b");
        // Two written to at the same time are two programs.
        let both = [file("a", 1100, 2600), file("b", 2000, 2500)];
        let why = pick(&both, &[]).unwrap_err();
        assert!(why.contains("more than one Codex is at work here"), "{why}");
        // Another pane of the same kind started at 1500. What began before
        // that is this pane's; what began after could be either's.
        assert_eq!(pick(&[file("a", 1100, 1200)], &[1500]).unwrap(), "a");
        let why = pick(&[file("b", 2000, 2500)], &[1500]).unwrap_err();
        assert!(why.contains("could be either pane's"), "{why}");
        // This pane's own goes on while the other pane's begins: its own.
        assert_eq!(pick(&both, &[1500]).unwrap(), "a");
        // Its own went quiet and a later one began: `/new` here, or the
        // other pane's first. Not known, so it keeps the one that is its.
        assert_eq!(pick(&moved, &[1500]).unwrap(), "a");
        // A pane that started before this one could have begun it too.
        let why = pick(&[file("a", 1100, 1200)], &[900]).unwrap_err();
        assert!(why.contains("could be either pane's"), "{why}");
    }
}
