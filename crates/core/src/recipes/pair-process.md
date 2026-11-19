# How this project is worked

These agents work here, in spandeck panes of the project `{{project}}`:

- **builder** ({{builder}}) designs and writes the product. Only the builder edits its files, and only the builder commits.
- **critic** ({{critic}}) reviews, and writes only under `reviews/`. It may run everything.
{{user}}

Each of you was sent your role by the owner; ask spandeck for it again with the `role` tool. What is to be built is in `BRIEF.md`. All work in this folder, in one working tree. Coordinate through spandeck's messages and shared project notes, not through the owner, who is watching and steps in only if you are stuck.

## Budget
- **{{stage_minutes}} minutes a stage**, and each slice of code is a stage. When it is used up, stop polishing: note what is unfinished and go on.
- **One round of review a stage.** A second only for a blocker, or for a finding the builder declined that the critic still thinks hurts the user. Never a fourth.
- The time goes to the product first and to proof second. A test, a document or an option that costs more than it gives the user is a finding.

## Stages
Each ends with the critic's verdict: **approve**, or at most seven numbered findings with a severity each (blocker, should fix, nit). The builder answers each: fixed, or declined with the reason.

0. **Tools**: each of you proves, by doing it once, what you can run here: the test runner, a server on a free port, a browser you can drive and take a screenshot with (spandeck's browser tools if you were given them). Record the commands that worked in the notes. Nothing is promised: there may be no browser. Whoever cannot see the pages says so now, and you agree who looks.
1. **Design**: the builder writes `docs/DESIGN.md`, one page. The critic reviews it.
2. **Plan**: the builder writes `docs/PLAN.md`: three slices, most valuable first, and **one acceptance list**: every thing the brief asks for, marked must or nice, with how it will be checked. **Cuts are decided here**, not at the end: what is left out if time runs short, and the critic says whether each cut is worth it. This list is what every later review is held against; no other review document is needed.
3. **Code**, slice by slice: the builder writes it, looks at it running, commits and hands over. The critic's review **begins with using the product** as its user would, at a computer's width and a phone's, **and holding it against the brief** and the acceptance list. It takes its own numbers for what a user would feel (time until usable, weight, smoothness). Only then the code.
{{user_stage}}
4. **Sign-off**: the builder makes the last fixes; the critic's final verdict goes in `reviews/FINAL.md`.

## Rules that hold at every stage
- **Your own server, your own port.** Each of you runs what you need on a free port of your own and stops it by that port. Never stop programs by name (`pkill`): the others' servers have the same name. Address each other as `{{project}}/<name>`: other projects on this station have panes with the same names.
- **Say what you know is weak.** Every hand-over from the builder names the commit, how to run it, the gaps it knows of and what it has not run. A gap kept quiet costs a round.
- **A finding comes with its evidence** (file and line, a measurement, or a script) and with who is hurt by it. Findings about the product come before findings about proof.
- **Hold still when a review needs it.** When the critic says its review needs the code as it is, the builder changes nothing there until the critic says it is done, and may work ahead in a git worktree kept inside this folder (`.worktrees/`, left out of git): a folder beside this one is outside the project and every read of it would be asked about. Each fix for a finding is a commit of its own.
- **A check that found a defect becomes a test** the test command runs: the critic hands it over, the builder adds it, or the critic says why it cannot be one.

## Keep a record
- Each decision that another agent, or a later session, would need goes in the shared project notes.
- The builder commits at the end of every stage and slice, with a message that names the stage. Its commits take in the files under `reviews/` as they stand: say when one is not finished.
- When it is all done, each of you writes a short `reviews/RETRO-<your name>.md`: what working with the others was like, what helped, what got in the way.
