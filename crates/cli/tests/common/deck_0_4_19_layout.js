// The Control Deck of spandeck 0.4.19, as it reads a saved layout and writes
// it again: crates/web/assets/app.js at 0c59986, these lines unchanged. Kept
// for tests/viewpanes.rs, which puts a layout with views through it. Do not
// edit: it is what that version does.
const isLeaf = (n) => !!n && n.dir === undefined;
const leaves = (n, out = []) => { if (!n) return out; if (isLeaf(n)) out.push(n); else { leaves(n.a, out); leaves(n.b, out); } return out; };
const cleanLeaf = (n) => n.id !== undefined ? { id: n.id, ...(n.station ? { station: n.station } : {}) }
  : { pane: n.pane, ...(n.project ? { project: n.project } : {}), ...(n.station ? { station: n.station } : {}) };
const leafSig = (n) => JSON.stringify(cleanLeaf(n));
// Is any of this part on screen?
const shows = (n) => !!n && (isLeaf(n) ? !n.hidden : shows(n.a) || shows(n.b));
const fromServer = (n) => !n ? null : isLeaf(n) ? cleanLeaf(n) : { dir: n.dir, ratio: n.ratio / 1000, a: fromServer(n.a), b: fromServer(n.b) };
const toServer = (n) => !n ? null : isLeaf(n) ? cleanLeaf(n)
  : { dir: n.dir, ratio: Math.min(950, Math.max(50, Math.round(n.ratio * 1000))), a: toServer(n.a), b: toServer(n.b) };

// A saved layout's tree, its hidden panes marked; and a layout as it is saved.
function treeFrom(def) {
  const tree = fromServer(def.tree), hid = new Set((Array.isArray(def.hidden) ? def.hidden : []).filter(isLeaf).map(leafSig));
  for (const n of leaves(tree)) if (hid.has(leafSig(n))) n.hidden = true;
  return tree;
}
function layoutBody(name, c) {
  const hidden = leaves(c.tree).filter(n => n.hidden).map(cleanLeaf);
  return { name, lines: !!c.lines, tree: toServer(c.tree), ...(hidden.length ? { hidden } : {}) };
}
