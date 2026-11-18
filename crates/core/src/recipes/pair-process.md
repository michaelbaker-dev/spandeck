# How this project is worked

Two agents work here, in spandeck panes of the project `{{project}}`:

- **builder** ({{builder}}) designs and writes the app. Only the builder edits the app's files.
- **critic** ({{critic}}) reviews: architecture, plan, code and UX. The critic writes only under `reviews/`
  and never edits the builder's files. It may run the app and its tests.

Both work in this folder, in the same working tree. What is to be built is in `BRIEF.md`.

Coordinate with each other through spandeck's own tools (messages, the shared project
notes, recall), not through the owner. The owner is watching and steps in only if you are stuck.

## Stages
Each stage ends with the critic's verdict: **approve**, or a numbered list of findings
with a severity each (blocker, should fix, nit). The builder answers every finding
(fixed, or declined with the reason) before the next stage starts. At most three rounds
a stage: after that, record what is still disagreed and move on.

0. **Tools**: before anything is designed, each of you finds out what you can really run on
   this machine, and proves it by doing it once: a browser you can drive headless and take a
   screenshot with, the test runner, a free port to serve the app on. Record what worked, with
   the command, in the shared project notes. Nothing is promised here: there may be no browser
   you can drive. If the builder cannot see its own pages it says so now, and the two of you
   agree who checks the pages and when (for example: the critic checks each slice's page as
   it is committed, not only at the UX stage). The stage ends when both lists and that
   agreement are in the notes and the critic approves them.
1. **Architecture**: builder writes `docs/DESIGN.md` (one page). Critic reviews it.
2. **Plan**: builder writes `docs/PLAN.md`: ordered steps, in three slices, with the tests for each. Critic reviews it.
3. **Code**: for each of the three slices, builder writes it and commits; critic reviews the commit by reading and running it.
4. **UX**: critic reviews the finished pages: layout, wording, phone width, keyboard use, accessibility.
5. **Sign-off**: builder makes the last fixes; critic gives a final verdict in `reviews/FINAL.md`.

## Rules that hold at every stage
- **Agree early on what "done" is.** The critic's verdicts on the architecture and on the plan
  each list what the critic will verify later. The plan carries a test for every item on those lists.
- **Say what you know is weak.** Every hand-over message from the builder lists the gaps it
  knows of and anything it has not run. A gap the builder kept to itself and the critic then
  found has cost a round.
- **A finding comes with its evidence**: the file and line, a measurement, or a script that
  reproduces it.
- **Hold still when a review needs it.** When the critic says its review needs the code to
  stay as it is (a review in a browser does, so that page and code stay in step), the builder
  changes nothing until the critic says it is done. Otherwise the builder may work ahead on
  the next slice, as long as each fix for a finding is a commit of its own.
- **A check that found a defect outlives the review.** The critic hands it to the builder,
  who makes it a test that the test command runs; or the critic says why it cannot be one.

## Keep a record
- Record each decision that the other agent, or a later session, would need, in the shared project notes.
- The builder commits at the end of every stage and slice, with a message that names the stage.
- Only the builder commits. Its commits take in the critic's files under `reviews/` as they stand, so the critic
  says when a review file is not finished.
- When the whole thing is done, each of you writes a short `reviews/RETRO-<your name>.md`:
  what working with the other agent was like, what helped, what got in the way.
