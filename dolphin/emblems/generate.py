"""Writes the emblems the overlay plugin draws, at 8, 16, 22 and 32 px, into the
directories beside this script (dolphin/CMakeLists.txt installs them):

    python3 dolphin/emblems/generate.py

Each emblem sits on a white plate, so it reads as a badge on any file icon and on both
Breeze and Breeze Dark backgrounds. The plate is a fixed white, as on Windows: KIconLoader
recolours only icons of a theme with FollowsColorScheme=true, and these ship in hicolor.
The status colours are fixed (Breeze's blue, green and red) so that a user's accent colour
can never turn the sync arrows green. The 8 px set, which every icon of the Details view
gets, is drawn on the pixel grid.
"""
import math, os, sys

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
BLUE = "#1d99f3"
GREEN = "#27ae60"
RED = "#da4453"
WHITE = "#ffffff"

# Per size: plate halo, outline stroke, glyph stroke.
SPEC = {
    8:  dict(halo=0.5, stroke=1.0, glyph=1.1),
    16: dict(halo=1.0, stroke=1.5, glyph=1.6),
    22: dict(halo=1.25, stroke=2.0, glyph=2.1),
    32: dict(halo=1.5, stroke=2.5, glyph=3.0),
}

HEAD = """<svg xmlns="http://www.w3.org/2000/svg" width="{s}" height="{s}" viewBox="0 0 {s} {s}">
"""

def f(x):
    return ("%.3f" % x).rstrip("0").rstrip(".")

def plate_circle(s):
    c = s / 2
    return f'  <circle cx="{f(c)}" cy="{f(c)}" r="{f(c)}" fill="#ffffff"/>\n'

def check(points, color, width):
    d = "M" + " L".join(f"{f(x)} {f(y)}" for x, y in points)
    return f'  <path d="{d}" fill="none" stroke="{color}" stroke-width="{f(width)}" stroke-linecap="round" stroke-linejoin="round"/>\n'

def downloaded(s):
    p = SPEC[s]; c = s / 2
    r_out = c - p["halo"]; r = r_out - p["stroke"] / 2
    inner = r_out - p["stroke"]
    k = inner * 0.62
    pts = [(c - k * 1.0, c + k * 0.08), (c - k * 0.28, c + k * 0.78), (c + k * 1.0, c - k * 0.55)]
    body = plate_circle(s)
    body += f'  <circle cx="{f(c)}" cy="{f(c)}" r="{f(r)}" fill="none" stroke="{GREEN}" stroke-width="{f(p["stroke"])}"/>\n'
    body += check(pts, GREEN, p["glyph"] * (0.95 if s > 8 else 0.9))
    return body

def always_keep(s):
    p = SPEC[s]; c = s / 2
    r = c - p["halo"]
    k = r * 0.52
    pts = [(c - k * 1.0, c + k * 0.06), (c - k * 0.3, c + k * 0.76), (c + k * 1.0, c - k * 0.58)]
    body = plate_circle(s)
    body += f'  <circle cx="{f(c)}" cy="{f(c)}" r="{f(r)}" fill="{GREEN}"/>\n'
    body += check(pts, WHITE, p["glyph"])
    return body

def syncing(s, frame=0):
    p = SPEC[s]; c = s / 2
    r = c - p["halo"]
    body = plate_circle(s)
    body += f'  <circle cx="{f(c)}" cy="{f(c)}" r="{f(r)}" fill="{BLUE}"/>\n'
    ra = r * 0.52
    w = p["glyph"] * (0.85 if s > 8 else 0.9)
    def pt(deg, rad=ra):
        t = math.radians(deg)
        return (c + rad * math.cos(t), c + rad * math.sin(t))
    if frame:
        body += f'  <g transform="rotate({f(frame * 22.5)} {f(c)} {f(c)})">\n'
    # 120 degree arcs, so a clear gap parts each arrowhead from the next arc's tail.
    for start, end in ((195, 315), (15, 135)):
        x0, y0 = pt(start); x1, y1 = pt(end)
        body += f'  <path d="M{f(x0)} {f(y0)} A{f(ra)} {f(ra)} 0 0 1 {f(x1)} {f(y1)}" fill="none" stroke="{WHITE}" stroke-width="{f(w)}" stroke-linecap="butt"/>\n'
        # Arrowhead at the end of the arc, pointing on clockwise.
        t = math.radians(end)
        tx, ty = -math.sin(t), math.cos(t)          # tangent (clockwise in y-down)
        nx, ny = math.cos(t), math.sin(t)           # radial
        half = w * 1.25
        tip = (x1 + tx * w * 1.7, y1 + ty * w * 1.7)
        a = (x1 + nx * half, y1 + ny * half)
        b = (x1 - nx * half, y1 - ny * half)
        body += f'  <path d="M{f(a[0])} {f(a[1])} L{f(tip[0])} {f(tip[1])} L{f(b[0])} {f(b[1])} Z" fill="{WHITE}"/>\n'
    if frame:
        body += '  </g>\n'
    return body

def error(s):
    p = SPEC[s]; h = p["halo"]
    body = f'  <rect width="{s}" height="{s}" rx="{f(s * 0.22)}" fill="#ffffff"/>\n'
    body += f'  <rect x="{f(h)}" y="{f(h)}" width="{f(s - 2 * h)}" height="{f(s - 2 * h)}" rx="{f(s * 0.16)}" fill="{RED}"/>\n'
    a = s * 0.33; b = s * 0.67
    w = p["glyph"]
    body += f'  <path d="M{f(a)} {f(a)} L{f(b)} {f(b)} M{f(b)} {f(a)} L{f(a)} {f(b)}" stroke="{WHITE}" stroke-width="{f(w)}" stroke-linecap="round"/>\n'
    return body

def cloud_circles(s, grow):
    """The cloud as a union of three circles and a bar, sat on the emblem's bottom edge."""
    p = SPEC[s]
    margin = p["halo"] + p["stroke"] / 2
    left, right, bottom = margin, s - margin, s - margin
    w = right - left
    # Shape in unit width; bottom at y=0, up is negative.
    unit = [(0.20, -0.20, 0.20), (0.50, -0.42, 0.30), (0.78, -0.25, 0.25)]
    out = []
    for ux, uy, ur in unit:
        out.append(("c", left + ux * w, bottom + uy * w, ur * w + grow))
    out.append(("r", left + 0.20 * w - grow * 0, bottom - 0.30 * w, (0.78 - 0.20) * w, 0.30 * w + grow))
    return out

def shapes(items, fill):
    out = ""
    for it in items:
        if it[0] == "c":
            _, x, y, r = it
            out += f'  <circle cx="{f(x)}" cy="{f(y)}" r="{f(r)}" fill="{fill}"/>\n'
        else:
            _, x, y, w, h = it
            out += f'  <rect x="{f(x)}" y="{f(y)}" width="{f(w)}" height="{f(h)}" fill="{fill}"/>\n'
    return out

def online_only(s):
    p = SPEC[s]
    st = p["stroke"]; halo = p["halo"]
    body = shapes(cloud_circles(s, st / 2 + halo), WHITE)
    body += shapes(cloud_circles(s, st / 2), BLUE)
    body += shapes(cloud_circles(s, -st / 2), WHITE)
    return body

def px(cells, color):
    """Pixel art from rows of '#' and '.': one path for every cell, so that no seam shows
    between two cells when the emblem is drawn larger (a fractional scale)."""
    d = "".join(f"M{x} {y}h1v1h-1z" for y, row in enumerate(cells) for x, ch in enumerate(row) if ch == "#")
    return f'  <path d="{d}" fill="{color}"/>\n'


CLOUD8_EDGE = [
    "........",
    "........",
    "........",
    "...##...",
    ".##..#..",
    "#.....#.",
    "#......#",
    ".######.",
]
CLOUD8_FILL = [
    "........",
    "........",
    "........",
    "........",
    "...##...",
    ".#####..",
    ".######.",
    "........",
]
X8 = [
    "........",
    "........",
    "..#..#..",
    "...##...",
    "...##...",
    "..#..#..",
    "........",
    "........",
]

def online_only_8():
    # No halo at this size: around a cloud this small it reads as a white mound in Breeze Dark.
    return px(CLOUD8_EDGE, BLUE) + px(CLOUD8_FILL, WHITE)

def error_8():
    body = f'  <rect width="8" height="8" rx="1.5" fill="{WHITE}"/>\n'
    body += f'  <rect x="0.5" y="0.5" width="7" height="7" rx="1.25" fill="{RED}"/>\n'
    return body + px(X8, WHITE)

def downloaded_8():
    body = f'  <circle cx="4" cy="4" r="4" fill="{WHITE}"/>\n'
    body += f'  <circle cx="4" cy="4" r="3.0" fill="none" stroke="{GREEN}" stroke-width="1"/>\n'
    body += check([(2.5, 4.1), (3.6, 5.1), (5.5, 3.1)], GREEN, 1.0)
    return body

def always_keep_8():
    body = f'  <circle cx="4" cy="4" r="4" fill="{WHITE}"/>\n'
    body += f'  <circle cx="4" cy="4" r="3.5" fill="{GREEN}"/>\n'
    body += check([(2.3, 4.0), (3.5, 5.2), (5.7, 2.9)], WHITE, 1.05)
    return body

def syncing_8(frame=0):
    body = f'  <circle cx="4" cy="4" r="4" fill="{WHITE}"/>\n'
    body += f'  <circle cx="4" cy="4" r="3.5" fill="{BLUE}"/>\n'
    # Two short round-capped arcs on the ring of pixels around the centre, without the
    # arrowheads, which blur at this size; like the larger glyph, they repeat every half-turn.
    # The stroke is over 1 px: Qt rasterises thinner strokes unevenly once rotated, so the
    # frames would pulse.
    if frame:
        body += f'  <g transform="rotate({f(frame * 22.5)} 4 4)">\n'
    r = 1.75
    for start in (202.5, 22.5):
        (x0, y0), (x1, y1) = ((4 + r * math.cos(math.radians(a)), 4 + r * math.sin(math.radians(a))) for a in (start, start + 70))
        body += f'  <path d="M{f(x0)} {f(y0)} A{f(r)} {f(r)} 0 0 1 {f(x1)} {f(y1)}" fill="none" stroke="{WHITE}" stroke-width="1.3" stroke-linecap="round"/>\n'
    if frame:
        body += '  </g>\n'
    return body

EIGHT = {
    "konedrive-online-only": online_only_8,
    "konedrive-downloaded": downloaded_8,
    "konedrive-always-keep": always_keep_8,
    "konedrive-syncing": syncing_8,
    "konedrive-error": error_8,
}

EMBLEMS = {
    "konedrive-online-only": online_only,
    "konedrive-downloaded": downloaded,
    "konedrive-always-keep": always_keep,
    "konedrive-syncing": syncing,
    "konedrive-error": error,
}

for s in SPEC:
    d = os.path.join(OUT, f"{s}")
    os.makedirs(d, exist_ok=True)
    for name, fn in EMBLEMS.items():
        with open(os.path.join(d, name + ".svg"), "w") as fh:
            body = EIGHT[name]() if s == 8 else fn(s)
            fh.write(HEAD.format(s=s) + body + "</svg>\n")
    for frame in range(1, 8):
        with open(os.path.join(d, f"konedrive-syncing-{frame}.svg"), "w") as fh:
            body = syncing_8(frame) if s == 8 else syncing(s, frame)
            fh.write(HEAD.format(s=s) + body + "</svg>\n")
