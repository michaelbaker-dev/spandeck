//! Stations' names: former names, and telling two names apart.
//!
//! A station has no identity but its name (DECISIONS, "A station is
//! renamed, and no two stations have one name"). So a rename is remembered
//! as a pair, old name and new, with the time it was made; every station of
//! the fleet keeps the pairs and tells the others, and a name that is a
//! former name of a station still finds it. Nothing here touches a station:
//! this is the rule, apart from the daemon.

use serde::{Deserialize, Serialize};

/// What a station that knows of renames announces (`caps`), and the name of
/// its switch (`[flags] rename = false` makes a station behave as one from
/// before, in both directions).
pub const CAP: &str = "rename";

/// A former name is remembered this long, on every station. Until then it
/// finds the station, and no other station may take it.
pub const KEPT_MS: u64 = 365 * 24 * 3_600_000;

/// At most this many pairs are kept, and taken from another station.
pub const MAX: usize = 64;

/// One rename: the station called `from` is called `to` since `at_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renamed {
    pub from: String,
    pub to: String,
    pub at_ms: u64,
}

/// May a station be called this? The rule a name already had to meet
/// wherever it is stored (a layout's leaf, a pin, the roster): letters,
/// digits, `-`, `_` and `.`, 64 at most, not beginning with a dot.
pub fn valid(name: &str) -> bool {
    crate::project::valid_name(name)
}

/// The same name, capitals aside?
pub fn alike(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The one of `names` that `asked` means: the name exactly as written if
/// there is one, otherwise a name that differs only by capitals, and only
/// when exactly one does. Two stations called `RTX6000` and `rtx6000` are
/// each found by their own spelling, and `Rtx6000` finds neither.
pub fn pick<'a>(names: impl IntoIterator<Item = &'a str>, asked: &str) -> Option<&'a str> {
    let mut near = None;
    let mut count = 0;
    for name in names {
        if name == asked {
            return Some(name);
        }
        if alike(name, asked) {
            near = Some(name);
            count += 1;
        }
    }
    near.filter(|_| count == 1)
}

/// The renames a station knows: one pair for each former name, the newest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names(Vec<Renamed>);

impl Names {
    pub fn from_list(list: Vec<Renamed>) -> Names {
        let mut names = Names::default();
        for r in list {
            names.add(r);
        }
        names
    }

    pub fn list(&self) -> &[Renamed] {
        &self.0
    }

    /// Take a pair. Says whether it is news: a former name not known
    /// before, or a later rename of one that was. A pair that names nothing
    /// usable is not taken.
    pub fn add(&mut self, r: Renamed) -> bool {
        if !valid(&r.from) || !valid(&r.to) || r.from == r.to {
            return false;
        }
        match self.0.iter().position(|k| k.from == r.from) {
            Some(i) if self.0[i].at_ms >= r.at_ms => return false,
            Some(i) => self.0[i] = r,
            None if self.0.len() >= MAX => return false,
            None => self.0.push(r),
        }
        self.0
            .sort_by(|a, b| (a.at_ms, &a.from).cmp(&(b.at_ms, &b.from)));
        true
    }

    /// Forget what is older than `KEPT_MS`. Says whether anything went.
    pub fn prune(&mut self, now_ms: u64) -> bool {
        let before = self.0.len();
        self.0.retain(|r| now_ms.saturating_sub(r.at_ms) < KEPT_MS);
        self.0.len() != before
    }

    /// What the station once called `name` is called now: each rename is
    /// followed to the next, always to a later one, so a station that was
    /// renamed and then renamed back is found under either name as what it
    /// is called now. A name that is nobody's former name is itself.
    pub fn now_called(&self, name: &str) -> String {
        let (mut current, mut after) = (name, None::<u64>);
        for _ in 0..MAX {
            let next = self
                .0
                .iter()
                .find(|r| r.from == current && after.is_none_or(|t| r.at_ms > t));
            match next {
                Some(r) => (current, after) = (r.to.as_str(), Some(r.at_ms)),
                None => break,
            }
        }
        current.to_string()
    }

    /// Is `name` a former name of a station that is called something else
    /// now? That name, if so.
    pub fn former(&self, name: &str) -> Option<String> {
        Some(self.now_called(name)).filter(|now| now != name)
    }

    /// The former names of the station called `name` now, newest first.
    pub fn former_of(&self, name: &str) -> Vec<String> {
        let mut out: Vec<&Renamed> = self
            .0
            .iter()
            .filter(|r| r.from != name && self.now_called(&r.from) == name)
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
        out.into_iter().map(|r| r.from.clone()).collect()
    }

    /// A time for a rename made now: later than the clock and than every
    /// rename already known, so that a clock that is slow does not put a
    /// rename before the one it undoes.
    pub fn stamp(&self, now_ms: u64) -> u64 {
        self.0
            .iter()
            .map(|r| r.at_ms + 1)
            .max()
            .unwrap_or(0)
            .max(now_ms)
    }
}

/// Why the station called `me` may not be renamed to `new`, as a sentence
/// for the owner; `None` when it may. `others`: every other station this
/// one lists (members, and machines that have not joined), by the name
/// each has now. `names`: the renames remembered.
pub fn refusal(new: &str, me: &str, others: &[String], names: &Names) -> Option<String> {
    if new.is_empty() {
        return Some("A station needs a name.".into());
    }
    if !valid(new) {
        return Some(format!(
            "\"{}\" cannot be a station's name: a name is letters, digits, - _ and . only, 64 at most, and does not begin with a dot.",
            new.chars().filter(|c| !c.is_control()).take(80).collect::<String>()
        ));
    }
    if new == me {
        return Some(format!("This station is already called {me}."));
    }
    let other = others
        .iter()
        .find(|o| o.as_str() == new)
        .or_else(|| others.iter().find(|o| alike(o, new) && o.as_str() != me));
    if let Some(other) = other {
        return Some(if other == new {
            format!("Another station is already called {other}.")
        } else {
            format!("Another station is called {other}, which differs from {new} only by capitals: choose a name that cannot be taken for it.")
        });
    }
    // A former name, still remembered, of another station. (This station's
    // own former names are free to it: that is renaming back.)
    for r in names.list() {
        let owner = names.now_called(&r.from);
        if alike(&r.from, new) && owner != me && !alike(&owner, new) {
            return Some(format!(
                "{} was {owner}'s name until it was renamed, and still finds it: choose another name.",
                r.from
            ));
        }
    }
    None
}

/// A name for a station whose own name is taken: `preferred` if it is free
/// (the machine's Tailscale name, say), else the name with `-2`, `-3`…
pub fn suggest(name: &str, preferred: Option<&str>, taken: &[String], names: &Names) -> String {
    let free = |n: &str| {
        valid(n)
            && !taken.iter().any(|t| alike(t, n))
            && !names.list().iter().any(|r| alike(&r.from, n))
    };
    if let Some(p) = preferred.filter(|p| free(p)) {
        return p.to_string();
    }
    let base: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(60)
        .collect();
    let base = base.trim_start_matches('.');
    let base = if base.is_empty() { "station" } else { base };
    (2..1000)
        .map(|n| format!("{base}-{n}"))
        .find(|n| free(n))
        .unwrap_or_else(|| format!("{base}-new"))
}

/// The station's config file with its `station =` line saying `new`, and
/// nothing else changed: the line is replaced where it stands, or, when
/// the file has none (the name was the machine's own), put first, where a
/// key belongs to no table. `Err` when the result would not read back as
/// that name: nothing is then written.
pub fn config_with_name(text: &str, new: &str) -> Result<String, String> {
    if !valid(new) {
        return Err("not a station's name".into());
    }
    let line = format!("station = \"{new}\"");
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    let mut done = false;
    let mut in_table = false;
    for raw in text.split_inclusive('\n') {
        let body = raw.trim_end_matches(['\n', '\r']);
        let ending = &raw[body.len()..];
        let trimmed = body.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        let is_key = !in_table
            && !done
            && trimmed
                .strip_prefix("station")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_key {
            // A comment after the value stays.
            let comment = toml_comment(body).unwrap_or("");
            out.push_str(&line);
            if !comment.is_empty() {
                out.push(' ');
                out.push_str(comment);
            }
            out.push_str(ending);
            done = true;
        } else {
            out.push_str(raw);
        }
    }
    if !done {
        out = format!("{line}\n{text}");
    }
    #[derive(Deserialize)]
    struct Named {
        station: Option<String>,
    }
    match toml::from_str::<Named>(&out) {
        Ok(read) if read.station.as_deref() == Some(new) => Ok(out),
        Ok(_) => Err("the config file's station line could not be found again".into()),
        Err(e) => Err(format!("the config file would not read: {e}")),
    }
}

/// The `# comment` that ends a line of TOML holding one quoted value.
fn toml_comment(line: &str) -> Option<&str> {
    let (mut quote, mut escaped) = (None::<char>, false);
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => return Some(&line[i..]),
            _ => {}
        }
        escaped = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(from: &str, to: &str, at_ms: u64) -> Renamed {
        Renamed {
            from: from.into(),
            to: to.into(),
            at_ms,
        }
    }

    // The incident of 20 Nov 2026: a fleet listed RTX6000 and rtx6000.
    #[test]
    fn two_names_that_differ_only_by_capitals_are_each_found_by_their_own_spelling() {
        let fleet = ["RTX6000", "rtx6000", "spark2"];
        assert_eq!(pick(fleet, "rtx6000"), Some("rtx6000"));
        assert_eq!(pick(fleet, "RTX6000"), Some("RTX6000"));
        // Neither spelling: it could be either, so it is neither.
        assert_eq!(pick(fleet, "Rtx6000"), None);
        // Where only one station could be meant, capitals do not matter.
        assert_eq!(pick(fleet, "SPARK2"), Some("spark2"));
        assert_eq!(pick(fleet, "golf"), None);
    }

    #[test]
    fn a_former_name_finds_the_station_through_a_second_rename_and_a_rename_back() {
        let mut names = Names::default();
        assert!(names.add(pair("RTX6000", "rtx6000-wsl", 10)));
        assert_eq!(names.now_called("RTX6000"), "rtx6000-wsl");
        assert_eq!(names.now_called("rtx6000-wsl"), "rtx6000-wsl");
        assert_eq!(names.now_called("spark2"), "spark2");
        // A second rename: both former names find it.
        assert!(names.add(pair("rtx6000-wsl", "wsl", 20)));
        assert_eq!(names.now_called("RTX6000"), "wsl");
        assert_eq!(names.former_of("wsl"), vec!["rtx6000-wsl", "RTX6000"]);
        // Back to its first name: that is its name, and the others find it.
        assert!(names.add(pair("wsl", "RTX6000", 30)));
        assert_eq!(names.now_called("RTX6000"), "RTX6000");
        assert_eq!(names.now_called("rtx6000-wsl"), "RTX6000");
        assert_eq!(names.now_called("wsl"), "RTX6000");
        assert_eq!(names.former("RTX6000"), None);
        // The same pairs in any order, as two stations tell a third.
        let other = Names::from_list(vec![
            pair("wsl", "RTX6000", 30),
            pair("RTX6000", "rtx6000-wsl", 10),
            pair("rtx6000-wsl", "wsl", 20),
        ]);
        assert_eq!(other, names);
        // Told again, or told something older: not news.
        assert!(!names.add(pair("wsl", "RTX6000", 30)));
        assert!(!names.add(pair("wsl", "elsewhere", 5)));
        // A rename made now is later than all of them, whatever the clock.
        assert_eq!(names.stamp(7), 31);
        assert!(names.prune(31 + KEPT_MS));
        assert_eq!(names.now_called("wsl"), "wsl");
    }

    #[test]
    fn a_name_is_refused_in_a_sentence_when_another_station_has_or_had_it() {
        let others = vec!["rtx6000".to_string(), "spark2".to_string()];
        let mut names = Names::default();
        names.add(pair("m5", "MacBookM5", 5));
        let no = |new: &str| refusal(new, "RTX6000", &others, &names);
        assert_eq!(no("rtx6000-wsl"), None);
        assert!(no("rtx6000")
            .unwrap()
            .contains("Another station is already called rtx6000"));
        assert!(no("Spark2")
            .unwrap()
            .contains("differs from Spark2 only by capitals"));
        assert!(no("RTX6000").unwrap().contains("already called RTX6000"));
        assert!(no("has space").unwrap().contains("letters, digits"));
        assert!(no("").unwrap().contains("needs a name"));
        // Another station's former name is still that station's.
        let others = vec!["MacBookM5".to_string()];
        assert!(refusal("M5", "RTX6000", &others, &names)
            .unwrap()
            .contains("was MacBookM5's name"));
        // Its own former name is free to it: renaming back.
        assert_eq!(refusal("m5", "MacBookM5", &[], &names), None);
    }

    #[test]
    fn a_newcomer_whose_name_is_taken_is_offered_its_tailscale_name_or_a_number() {
        let taken = vec!["rtx6000".to_string(), "spark2".to_string()];
        let names = Names::default();
        assert_eq!(
            suggest("RTX6000", Some("rtx6000-wsl"), &taken, &names),
            "rtx6000-wsl"
        );
        assert_eq!(
            suggest("RTX6000", Some("spark2"), &taken, &names),
            "RTX6000-2"
        );
        assert_eq!(suggest("RTX6000", None, &taken, &names), "RTX6000-2");
    }

    // The config file is the owner's: a rename changes one line of it.
    #[test]
    fn only_the_station_line_of_the_config_file_changes() {
        let text = "# my station\nstation = \"RTX6000\"  # the WSL one\nweb_port = 7391\n\n[agents.claude]\nstation = \"not this\"\n";
        assert_eq!(
            config_with_name(text, "rtx6000-wsl").unwrap(),
            "# my station\nstation = \"rtx6000-wsl\" # the WSL one\nweb_port = 7391\n\n[agents.claude]\nstation = \"not this\"\n"
        );
        // No line: the name was the machine's own. It is put first, since a
        // key written after a table would belong to the table.
        let text = "web_port = 7391\r\n[web]\r\nhttps = true\r\n";
        let out = config_with_name(text, "rtx6000-wsl").unwrap();
        assert_eq!(out, format!("station = \"rtx6000-wsl\"\n{text}"));
        assert_eq!(config_with_name("", "a").unwrap(), "station = \"a\"\n");
        // Written in single quotes, with odd spacing, with a # in the name.
        assert_eq!(
            config_with_name("  station='old#1'\n", "new").unwrap(),
            "station = \"new\"\n"
        );
        // A file that does not read is left alone.
        assert!(config_with_name("station = \"a\"\n[broken\n", "b").is_err());
        assert!(config_with_name("station = \"a\"\n", "b c").is_err());
    }
}
