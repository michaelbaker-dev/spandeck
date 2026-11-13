# Manual checklist for a beta (ten minutes)

Things automation checks poorly. Run on the fleet after `scripts/check.sh` and
`scripts/canary.sh` pass.

- [ ] Mouse: click to focus, drag to resize, right-click menus feel right
- [ ] Phone: status, blockers, an approval and a stop all work
- [ ] Zoom from fleet to station to pane and back
- [ ] An agent running before the update is still running after it (`spandeck upgrade`)
- [ ] One delegated task with a review loop completes end to end
- [ ] `spandeck doctor` is clean on every station
