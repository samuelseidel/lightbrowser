"""Pointer-stroke geometry for the detect-bench: strokes from a recorded trace,
their normalised shapes, and when two shapes are the same stroke twice.

Used by `beh_path_repeat` (two strokes of one session replay the same path)
and by `/api/clusters` (sessions that share a stroke shape hash). Defender
side only: it reads what `collector.js` recorded.

A stroke is a run of trusted `mousemove` events with no pause over 250 ms and
no button press inside it. Its shape is the path resampled to 32 points by
arc length, moved so it starts at the origin, rotated so the start-to-end
chord lies on the x axis and scaled so the chord has length 1 (translation,
rotation and scale normalised). `bend` is how far the path leaves the chord.

Why a replay is told from two human strokes: a person repeating a gesture
differs from the first take by a good part of the stroke's own bend (hand
tremor, timing, speed). In the bench's data, 35 informative strokes from the
eight LightBrowser human-input runs, T-020 and T-003 (595 pairs, none of them
a replay), the closest two independent strokes are 1.5 px apart (RMS, after
normalising); a stored stroke played again differs only by pixel rounding,
well under 1 px. The threshold is 0.8 px, and at most 12 % of the smaller
bend. Strokes that stay near their chord carry no shape
(any two straight lines agree), so only strokes that bend by at least 6 px
and 2 % of their chord are compared; straight paths are `beh_mouse_linear`'s
business.
"""

from __future__ import annotations

import hashlib
import math
from bisect import bisect_right

GAP_MS = 250            # a longer pause ends a stroke
MIN_LEN_PX = 40.0       # strokes shorter than this are not compared
MIN_POINTS = 6
MIN_CHORD_PX = 20.0
MIN_CHORD_RATIO = 0.5   # chord / path length: no loops, no doubling back
MIN_BEND_PX = 6.0
MIN_BEND_RATIO = 0.02   # bend / chord
SAMPLES = 32
MAX_DIST_PX = 0.8       # RMS distance of two shapes, in px (at the pair's mean chord)
MAX_DIST_BEND = 0.12    # ... and at most this share of the smaller bend
HASH_STEP = 0.01        # shape hash quantisation, in chords
MAX_STROKES = 300       # strokes compared per session: the pairing is quadratic and a
                        # trace can hold tens of thousands of events (a hostile one on purpose)


def pointer_strokes(session: dict) -> list[list[tuple[float, float, float]]]:
    """Strokes of every page: lists of (x, y, t), in page arrival order."""
    out: list[list[tuple[float, float, float]]] = []
    pages = sorted(session.get("pages", {}).values(), key=lambda p: p.get("received", 0))
    for pg in pages:
        trace = pg.get("trace") if isinstance(pg, dict) else None
        ev = [e for e in trace if isinstance(e, dict) and e.get("tr")] if isinstance(trace, list) else []
        downs = sorted(e["t"] for e in ev if e.get("type") == "mousedown" and isinstance(e.get("t"), (int, float)))
        cur: list[tuple[float, float, float]] = []
        for e in ev:
            if e.get("type") != "mousemove" or not all(isinstance(e.get(k), (int, float)) for k in ("x", "y", "t")):
                continue
            if cur:
                last = cur[-1]
                pressed = bisect_right(downs, e["t"]) > bisect_right(downs, last[2])
                if e["t"] - last[2] > GAP_MS or pressed:
                    out.append(cur)
                    cur = []
            if cur and (e["x"], e["y"]) == cur[-1][:2]:
                continue
            cur.append((float(e["x"]), float(e["y"]), float(e["t"])))
        if cur:
            out.append(cur)
    return out


def normalise(stroke: list[tuple[float, float, float]]) -> dict | None:
    """The normalised shape of one stroke, or None when it is too short, too
    looped or too small to compare. `informative` is false for strokes that
    hug their chord."""
    if len(stroke) < MIN_POINTS:
        return None
    pts = [(p[0], p[1]) for p in stroke]
    seg = [math.dist(pts[i - 1], pts[i]) for i in range(1, len(pts))]
    length = sum(seg)
    chord = math.dist(pts[0], pts[-1])
    if length < MIN_LEN_PX or chord < MIN_CHORD_PX or chord < MIN_CHORD_RATIO * length:
        return None
    cum = [0.0]
    for d in seg:
        cum.append(cum[-1] + d)
    res = []
    j = 0
    for k in range(SAMPLES):
        target = length * k / (SAMPLES - 1)
        while j < len(seg) - 1 and cum[j + 1] < target:
            j += 1
        span = cum[j + 1] - cum[j]
        f = 0.0 if span <= 0 else min(1.0, max(0.0, (target - cum[j]) / span))
        res.append((pts[j][0] + f * (pts[j + 1][0] - pts[j][0]), pts[j][1] + f * (pts[j + 1][1] - pts[j][1])))
    x0, y0 = pts[0]
    theta = math.atan2(pts[-1][1] - y0, pts[-1][0] - x0)
    c, s = math.cos(theta), math.sin(theta)
    shape = [((c * (x - x0) + s * (y - y0)) / chord, (-s * (x - x0) + c * (y - y0)) / chord) for x, y in res]
    bend = max(abs(v) for _, v in shape) * chord
    return {
        "shape": shape,
        "length": length,
        "chord": chord,
        "bend": bend,
        "informative": bend >= MIN_BEND_PX and bend >= MIN_BEND_RATIO * chord,
        "t0": stroke[0][2],
    }


def distance(a: dict, b: dict) -> float:
    """RMS distance between two normalised shapes, in chords."""
    return math.sqrt(sum((p[0] - q[0]) ** 2 + (p[1] - q[1]) ** 2 for p, q in zip(a["shape"], b["shape"])) / SAMPLES)


def distance_px(a: dict, b: dict) -> float:
    """`distance` in pixels, at the mean chord of the two strokes."""
    return distance(a, b) * (a["chord"] + b["chord"]) / 2


def same_stroke(a: dict, b: dict) -> bool:
    """Two informative shapes closer than human repetition."""
    if not (a["informative"] and b["informative"]):
        return False
    return distance_px(a, b) <= min(MAX_DIST_PX, MAX_DIST_BEND * min(a["bend"], b["bend"]))


def repeated(session: dict) -> list[tuple[dict, dict, float]]:
    """Pairs of informative strokes of one session that are the same shape
    (among its first `MAX_STROKES` informative strokes)."""
    shapes = [n for n in (normalise(s) for s in pointer_strokes(session)) if n and n["informative"]][:MAX_STROKES]
    out = []
    for i in range(len(shapes)):
        for j in range(i + 1, len(shapes)):
            if same_stroke(shapes[i], shapes[j]):
                out.append((shapes[i], shapes[j], distance_px(shapes[i], shapes[j])))
    return out


def shape_hash(n: dict) -> str:
    """A hash of the quantised shape, for grouping sessions. Equal for a stroke
    replayed exactly; a stroke on the other side of a quantisation step hashes
    differently (use `same_stroke` within one session)."""
    step = SAMPLES // 16
    q = [round(n["shape"][k][1] / HASH_STEP) for k in range(0, SAMPLES, step)]
    return hashlib.sha1(repr(q).encode()).hexdigest()[:12]


def session_shape_hashes(session: dict) -> list[str]:
    """Hashes of the session's informative strokes (sorted, unique)."""
    hs = {shape_hash(n) for n in (normalise(s) for s in pointer_strokes(session)) if n and n["informative"]}
    return sorted(hs)
