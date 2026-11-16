// A finger on a terminal that the page has to follow: the arithmetic of a
// drag and of what comes after it.
//
// On a phone a terminal's history is scrolled by the phone itself, as a
// conversation's log is (hist.js; the owner, 16 Nov 2026: "have to fix
// terminal to be like chat"). What is left for the page to follow is a
// full-screen program, which keeps its own history and has to be told to
// scroll, and a terminal on a touch screen that is not a phone's, whose
// history the terminal still draws a line at a time. There the finger is
// tracked to the pixel, the view keeps moving after the finger lifts and
// slows as a phone's own scrolling does, and a touch stops it.
//
// Nothing here touches the page: term.js gives it the events and is told
// how far to move.

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
// A page that is not being drawn gets no frames, so a timer stands behind;
// whichever of the two comes first takes the other away. (Before 16 Nov 2026
// it did not: what was asked for again from inside `fn`, as a glide does
// every frame, left the other one armed, and that one ran `fn` again at
// once, which armed two more. Three drags kept the page's thread busy for
// 2.5 of 4.9 seconds: docs/measurements.md.)
export function framer(fn) {
  let asked = false, frame = 0, timer = 0;
  const run = () => {
    if (!asked) return;
    asked = false;
    if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(frame);
    clearTimeout(timer);
    fn(performance.now());
  };
  return () => {
    if (asked) return;
    asked = true;
    if (typeof requestAnimationFrame === 'function') frame = requestAnimationFrame(run);
    timer = setTimeout(run, 40);
  };
}
