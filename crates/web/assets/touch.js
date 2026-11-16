// A finger on a terminal: the arithmetic of a drag and of what follows it.
//
// A conversation's log is an element the phone scrolls itself, with the
// phone's own momentum. A terminal's history is not: its rows are drawn for
// one position, a line at a time, and a full-screen program keeps its own
// history and has to be told to scroll. So a drag there is followed here
// (the owner, 16 Nov 2026: "the Claude chat screen scrolls really nice but
// the antigravity terminal doesn't, is not that responsive"): the finger is
// tracked to the pixel, the view keeps moving after the finger lifts and
// slows as the phone's own scrolling does, and a touch stops it.
//
// Nothing here touches the page: term.js gives it the events and is told
// how far to move. (The browser could scroll the terminal's own viewport
// element by itself only if touches reached it, through the layer the text
// is drawn on; that layer is what a tap focuses, and a full-screen program
// has nothing there to scroll. So the drag is followed, for every program.)

// A drag is one way, the way it first went further: up and down, or sideways.
export const SLOP = 6;
export function axisOf(dx, dy, sideways) {
  if (Math.max(Math.abs(dx), Math.abs(dy)) < SLOP) return null;
  return sideways && Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
}

// How fast the finger was going when it lifted, in pixels a millisecond:
// over the last tenth of a second of the drag. `samples`: [{ t, x, y }],
// oldest first. A finger that rested before lifting, or a drag too short to
// tell, is no speed at all.
export const WINDOW_MS = 100, MAX_SPEED = 5, MIN_SPEED = 0.12;
export function speedOf(samples, now, axis) {
  const last = samples[samples.length - 1];
  if (!last || now - last.t > 60) return 0;
  const first = samples.find(s => last.t - s.t <= WINDOW_MS);
  const dt = last.t - first.t;
  if (dt < 12) return 0;
  const v = (last[axis] - first[axis]) / dt;
  return Math.abs(v) < MIN_SPEED ? 0 : Math.max(-MAX_SPEED, Math.min(MAX_SPEED, v));
}

// After the finger lifts: the speed falls off by the same share in every
// equal time (the time constant a phone's own scrolling uses, 325 ms), so
// whatever the rate of frames the view comes to rest in the same place,
// `speed * GLIDE_MS` pixels on. `dt` ms later: the speed then, and how far
// it went meanwhile.
export const GLIDE_MS = 325, REST_SPEED = 0.02;
export function glide(speed, dt) {
  const k = Math.exp(-Math.max(0, dt) / GLIDE_MS);
  return { speed: speed * k, moved: speed * GLIDE_MS * (1 - k) };
}

// Pixels dragged, as whole lines to move now and the part of a line left
// over, which is kept and drawn: nothing a finger does is dropped.
export function linesOf(px, line) {
  const whole = Math.trunc(px / line);
  return { whole, rest: px - whole * line };
}

// One thing to do in the next frame, however many times it is asked for.
// A page that is not being drawn gets no frames, so a timer stands behind.
export function framer(fn) {
  let asked = false;
  const run = () => { if (!asked) return; asked = false; fn(performance.now()); };
  return () => {
    if (asked) return;
    asked = true;
    if (typeof requestAnimationFrame === 'function') requestAnimationFrame(run);
    setTimeout(run, 40);
  };
}
