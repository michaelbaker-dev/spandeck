//! What an agent program printed on its terminal, as text to read
//! (DECISIONS.md, "One chat for every pane"; `docs/conversation-api.md`,
//! "From the screen").
//!
//! A terminal's rows are a drawing: indented, wrapped at the pane's width,
//! framed with rules, with the program's message box and status row under
//! them. The conversation view shows prose. This turns the rows into parts:
//! **prose**, to be drawn as markdown like any reply, and **mono**, rows
//! whose layout is their meaning (columns, a drawing), to be shown as they
//! were in a block that scrolls sideways.
//!
//! Only what can be told from the rows themselves is done:
//!
//! - a row the terminal wrapped at its right edge goes on in the next (the
//!   screen model says which: `Printed::joined`), so the two are one line;
//! - a row made only of rule characters is the program's frame, and is
//!   left out; a row between two bars keeps what is between them;
//! - on a screen the program draws itself, the frame the cursor is in is
//!   its message box, and the few rows under it are its status: left out;
//! - indentation that every row has is dropped, and a bullet the program
//!   drew (`•`, `●`) is a list item;
//! - a block (rows with no empty row between them) in which a row has
//!   box-drawing characters or columns set apart by runs of blanks stays
//!   mono.
//!
//! Nothing is guessed from the pane's width, and no word is changed. What
//! the rows were is still served beside the parts (`text`), and the
//! terminal has everything.

use serde::Serialize;

/// A run of what was printed. `prose`: markdown source, to be drawn like a
/// reply. Otherwise rows to be shown as they were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Part {
    pub prose: bool,
    pub text: String,
}

/// Rows under the program's message box that are taken for its status.
const STATUS_ROWS: usize = 3;

fn is_box_char(c: char) -> bool {
    ('\u{2500}'..='\u{259f}').contains(&c)
}

/// A row that is only a rule: the program's frame.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let marks = t.chars().filter(|c| !c.is_whitespace()).count();
    marks >= 3
        && t.chars().all(|c| {
            c.is_whitespace() || is_box_char(c) || matches!(c, '-' | '_' | '=' | '⎯' | '▔' | '▁')
        })
        // A markdown rule of three is a rule too, and nothing is lost.
        && !t.contains('|')
}

const BARS: [char; 7] = ['│', '┃', '║', '▌', '▐', '▎', '▍'];

/// What is inside a row's frame: between two bars, or after a bar at its
/// left. `None`: the row has no frame.
fn unframed(line: &str) -> Option<String> {
    let t = line.trim_end();
    let lead = t.len() - t.trim_start().len();
    let body = &t[lead..];
    let first = body.chars().next()?;
    if !BARS.contains(&first) {
        return None;
    }
    let mut inner = &body[first.len_utf8()..];
    let closed = inner.chars().last().is_some_and(|c| BARS.contains(&c));
    if closed {
        inner = &inner[..inner.len() - inner.chars().last().map_or(0, char::len_utf8)];
    } else if !(inner.is_empty() || inner.starts_with(' ')) {
        return None;
    }
    let inner = inner.strip_prefix(' ').unwrap_or(inner);
    Some(format!("{}{}", &t[..lead], inner.trim_end()))
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// A row whose layout is its meaning: a drawing, or columns.
fn aligned(line: &str) -> bool {
    let t = line.trim();
    if t.chars()
        .any(|c| is_box_char(c) || ('\u{2800}'..='\u{28ff}').contains(&c))
    {
        return true;
    }
    // Columns: text, a run of blanks, text. (After a list's mark or a
    // sentence's end two blanks are common; three are not.)
    t.contains("   ")
}

const BULLETS: [char; 9] = ['•', '◦', '▪', '‣', '●', '○', '∙', '·', '⏺'];

/// A bullet the program drew, as a list item.
fn bullet(line: &str) -> String {
    let lead = indent(line);
    let body = &line[lead..];
    let mut chars = body.chars();
    match (chars.next(), chars.next()) {
        (Some(b), Some(' ')) if BULLETS.contains(&b) => {
            format!("{}- {}", &line[..lead], chars.as_str().trim_start())
        }
        _ => line.to_string(),
    }
}

/// The rows of what a pane printed, as parts. `joined`: for each row,
/// whether it goes on from the row before. `cursor`: on a screen the
/// program draws itself, the row the cursor is on.
pub fn reflow(lines: &[String], joined: &[bool], cursor: Option<usize>) -> Vec<Part> {
    // One line for each line the program printed.
    let mut rows: Vec<String> = Vec::with_capacity(lines.len());
    let mut at_cursor = None;
    for (i, line) in lines.iter().enumerate() {
        match rows.last_mut() {
            Some(last) if joined.get(i) == Some(&true) => last.push_str(line),
            _ => rows.push(line.clone()),
        }
        if cursor == Some(i) {
            at_cursor = Some(rows.len() - 1);
        }
    }
    for row in rows.iter_mut() {
        let len = row.trim_end().len();
        row.truncate(len);
    }
    // The program's own frame. `None`: the row is left out.
    let framed: Vec<bool> = rows
        .iter()
        .map(|r| is_rule(r) || unframed(r).is_some())
        .collect();
    let mut keep: Vec<Option<String>> = rows
        .iter()
        .map(|r| {
            if is_rule(r) {
                None
            } else {
                Some(unframed(r).unwrap_or_else(|| r.clone()))
            }
        })
        .collect();
    if let Some(at) = at_cursor {
        // The frame the cursor is in is where the owner types (or, with no
        // frame, its row is the prompt): not something the program said.
        let (mut from, mut to) = (at, at);
        if framed[at] {
            while from > 0 && framed[from - 1] {
                from -= 1;
            }
            while to + 1 < rows.len() && framed[to + 1] {
                to += 1;
            }
        }
        let under = rows[to + 1..]
            .iter()
            .filter(|r| !r.trim().is_empty())
            .count();
        // With more than a few rows under it, those rows are no status
        // row, and a row with no frame is no prompt (a list of choices
        // has its cursor on one of them): they stay.
        let end = match (under <= STATUS_ROWS, framed[at]) {
            (true, _) => rows.len(),
            (false, true) => to + 1,
            (false, false) => from,
        };
        for k in keep.iter_mut().take(end).skip(from) {
            *k = None;
        }
    }
    // What is left, with empty rows where rows were left out, so that what
    // stood apart still does.
    let kept: Vec<String> = keep.into_iter().map(Option::unwrap_or_default).collect();
    let margin = kept
        .iter()
        .filter(|r| !r.trim().is_empty())
        .map(|r| indent(r))
        .min()
        .unwrap_or(0);
    let kept: Vec<&str> = kept
        .iter()
        .map(|r| {
            if r.trim().is_empty() {
                ""
            } else {
                &r[margin..]
            }
        })
        .collect();
    // Blocks: rows with no empty row between them. Inside a code fence
    // nothing is a block's end, and nothing is judged.
    let mut parts: Vec<Part> = vec![];
    let mut block: Vec<&str> = vec![];
    let mut fenced = false;
    let flush = |block: &mut Vec<&str>, parts: &mut Vec<Part>| {
        if block.is_empty() {
            return;
        }
        let code = block.iter().any(|r| r.trim_start().starts_with("```"));
        let prose = code || !block.iter().any(|r| aligned(r));
        let text = if prose {
            block
                .iter()
                .map(|r| bullet(r))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            block.join("\n")
        };
        match parts.last_mut() {
            Some(last) if last.prose == prose => {
                last.text.push_str(if prose { "\n\n" } else { "\n" });
                last.text.push_str(&text);
            }
            _ => parts.push(Part { prose, text }),
        }
        block.clear();
    };
    for row in kept {
        if row.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if row.is_empty() && !fenced {
            flush(&mut block, &mut parts);
        } else {
            block.push(row);
        }
    }
    flush(&mut block, &mut parts);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    /// The owner's report of 16 Nov 2026, with a picture of Antigravity's
    /// Chat: one box of terminal text, "indented as the terminal drew it,
    /// lines running off the right edge and cut, wrapped fragments mixed
    /// with indentation, markdown shown raw, bullets as ●". What an agent
    /// program drew becomes prose: wrapped rows are one line again, the
    /// frame, the message box and the status row are left out, the margin
    /// is dropped, and only rows whose layout is their meaning stay as
    /// they were.
    #[test]
    fn what_an_agent_drew_becomes_prose_and_what_is_laid_out_stays() {
        let screen = rows(
            "  ╭──────────────────────────────╮
  │ Tern Code   /w/mere          │
  ╰──────────────────────────────╯

  ### 5. Grey heron
  The grey heron stands in the sh
allows and waits for a fish.
  ● nests high in the alders
  ● feeds at dusk

  name      nests   seen
  heron     9       yes

  ╭──────────────────────────────╮
  │ > count the gre              │
  ╰──────────────────────────────╯
  tern-2 · 12% context · esc to stop",
        );
        let mut joined = vec![false; screen.len()];
        joined[6] = true;
        let parts = reflow(&screen, &joined, Some(14));
        let part = |prose, text: &str| Part {
            prose,
            text: text.into(),
        };
        assert_eq!(
            parts,
            vec![
                // The title row has columns of its own: it stays as it was.
                part(false, "Tern Code   /w/mere"),
                part(true, "### 5. Grey heron\nThe grey heron stands in the shallows and waits for a fish.\n- nests high in the alders\n- feeds at dusk"),
                part(false, "name      nests   seen\nheron     9       yes"),
            ]
        );
        // Without a cursor (a block of a log, cut above the prompt) nothing
        // is taken for a message box, and a frame keeps what is inside it.
        let log =
            rows("│ a note in a frame │\n\nplain words\n```\n  let x = 1;\n\n  x   +   1\n```");
        let parts = reflow(&log, &[], None);
        assert_eq!(
            parts,
            vec![part(
                true,
                "a note in a frame\n\nplain words\n```\n  let x = 1;\n\n  x   +   1\n```"
            )]
        );
        // A list of choices with the cursor on one of them: every row stays.
        let picker = rows(
            "Trust this folder?\n  1. Yes\n  2. No\n  3. Ask each time\n  4. Never\n  5. Help",
        );
        let parts = reflow(&picker, &[], Some(1));
        assert!(parts[0].text.contains("1. Yes"), "{parts:?}");
    }
}
