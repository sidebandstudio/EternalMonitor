#!/usr/bin/env python3
"""Draw the README's "How it works" diagram in dark and light variants.

GitHub shows README SVGs as images, which load no web fonts, so every label is
shaped with HarfBuzz from the site's Geist fonts (docs/assets/fonts) and
written as outlines. Edit the copy or layout here and run the script again;
don't edit the SVGs by hand.

The README shows the image about 830 px wide, so every supporting label is at
least 16.5 px (SMALL) on the 1120 px canvas, which displays as about 12 px.

Needs fonttools, brotli and uharfbuzz (pip install fonttools brotli uharfbuzz).
Writes docs/assets/readme/how-it-works-{dark,light}.svg.
"""
import io
import pathlib

import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

ROOT = pathlib.Path(__file__).resolve().parents[1]
FONTS = ROOT / "docs/assets/fonts"
OUT = ROOT / "docs/assets/readme"

W, H = 1120, 688
SMALL, TITLE, HEADING = 16.5, 19, 21

THEMES = {
    "dark": {
        "bg": "#0A0A0B",
        "card_border": "rgba(255,255,255,0.08)",
        "dots": "rgba(255,255,255,0.075)",
        "glow": "#E8FF47",
        "glow_opacity": 0.08,
        "frame": "#121214",
        "frame_stroke": "rgba(255,255,255,0.14)",
        "screen": "#0A0A0B",
        "screen_stroke": "rgba(255,255,255,0.06)",
        "hairline": "rgba(255,255,255,0.08)",
        "text": "#F4F4F5",
        "muted": "#A1A1AA",
        "faint": "#8F8F98",
        "video": "#E8FF47",
        "video_rail": "rgba(232,255,71,0.35)",
        "audio": "#C3D63B",
        "input": "#A1A1AA",
        "video_pill": "#E8FF47",
        "video_pill_stroke": "none",
        "video_pill_text": "#0C0D04",
        "audio_pill": "#1C1E10",
        "audio_pill_stroke": "rgba(232,255,71,0.55)",
        "audio_pill_text": "#E8FF47",
        "chip": "#121214",
        "chip_stroke": "rgba(255,255,255,0.16)",
        "chip_text": "#F4F4F5",
        "port": "#0A0A0B",
    },
    "light": {
        "bg": "#FFFFFF",
        "card_border": "rgba(0,0,0,0.09)",
        "dots": "rgba(0,0,0,0.08)",
        "glow": "#E8FF47",
        "glow_opacity": 0.14,
        "frame": "#F4F4F5",
        "frame_stroke": "rgba(0,0,0,0.13)",
        "screen": "#FFFFFF",
        "screen_stroke": "rgba(0,0,0,0.07)",
        "hairline": "rgba(0,0,0,0.08)",
        "text": "#09090B",
        "muted": "#52525B",
        "faint": "#63636B",
        "video": "#5C6B00",
        "video_rail": "rgba(92,107,0,0.40)",
        "audio": "#7C8F00",
        "input": "#71717A",
        "video_pill": "#E8FF47",
        "video_pill_stroke": "rgba(0,0,0,0.12)",
        "video_pill_text": "#0C0D04",
        "audio_pill": "#FFFFFF",
        "audio_pill_stroke": "#7C8F00",
        "audio_pill_text": "#3F4A00",
        "chip": "#FFFFFF",
        "chip_stroke": "rgba(0,0,0,0.14)",
        "chip_text": "#09090B",
        "port": "#FFFFFF",
    },
}


class Font:
    def __init__(self, filename):
        self.tt = TTFont(FONTS / filename)
        self.tt.flavor = None
        data = io.BytesIO()
        self.tt.save(data)
        self.hb = hb.Font(hb.Face(data.getvalue()))
        self.upem = self.tt["head"].unitsPerEm
        self.glyphs = self.tt.getGlyphSet()

    def shape(self, text):
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        hb.shape(self.hb, buf, {"kern": True, "liga": True})
        return list(zip(buf.glyph_infos, buf.glyph_positions))

    def width(self, text, size):
        return sum(p.x_advance for _, p in self.shape(text)) * size / self.upem

    def outline(self, text, size, x, y):
        scale = size / self.upem
        pen = SVGPathPen(self.glyphs, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
        for info, pos in self.shape(text):
            name = self.tt.getGlyphName(info.codepoint)
            gx = x + pos.x_offset * scale
            gy = y - pos.y_offset * scale
            self.glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, gx, gy)))
            x += pos.x_advance * scale
        return pen.getCommands()


REGULAR = Font("Geist-Regular.woff2")
MEDIUM = Font("Geist-Medium.woff2")
SEMIBOLD = Font("Geist-SemiBold.woff2")
MONO = Font("GeistMono-Regular.woff2")


def text(font, size, value, x, y, fill, anchor="start", max_width=None):
    width = font.width(value, size)
    if max_width is not None and width > max_width:
        raise SystemExit(f"{value!r} is {width:.0f}px wide, over its {max_width}px")
    if anchor == "middle":
        x -= width / 2
    elif anchor == "end":
        x -= width
    return f'<path fill="{fill}" d="{font.outline(value, size, x, y)}"/>'


# Row tops inside the screens. Both devices share them, so each lane's arrow
# runs straight across: video is three rows, audio and input one each.
VIDEO_ROWS = (136, 196, 256)
AUDIO_ROW = 348
INPUT_ROW = 440
DIVIDERS = (326, 418)
NODE = 11  # node centre below a row top

PC_ROWS = [
    ("video", "Capture", "The desktop, through DXGI", VIDEO_ROWS[0]),
    ("video", "Encode", "On the GPU, through FFmpeg", VIDEO_ROWS[1]),
    ("video", "Send", "Numbered fragments", VIDEO_ROWS[2]),
    ("audio", "Capture audio", "WASAPI loopback, encoded as Opus", AUDIO_ROW),
    ("input", "Inject input", "Mouse, keys and pen input on Windows", INPUT_ROW),
]
IPAD_ROWS = [
    ("video", "Reassemble and repair", "Rebuilds frames from fragments", VIDEO_ROWS[0]),
    ("video", "Decode", "In hardware with VideoToolbox", VIDEO_ROWS[1]),
    ("video", "Display", "Drawn with Metal", VIDEO_ROWS[2]),
    ("audio", "Play audio", "Opus decoded to AVAudioEngine", AUDIO_ROW),
    ("input", "Input", "Touch, Pencil, keyboard, trackpad", INPUT_ROW),
]

SCREEN_Y, SCREEN_H = 108, 408
PC = {"x": 40, "y": 94, "w": 410, "h": 450, "screen": (52, SCREEN_Y, 386, SCREEN_H)}
IPAD = {"x": 670, "y": 94, "w": 410, "h": 436, "screen": (684, SCREEN_Y, 382, SCREEN_H)}
LEFT, RIGHT = PC["x"] + PC["w"], IPAD["x"]
MID = (LEFT + RIGHT) / 2


def node(t, kind, x, y):
    if kind == "audio":
        return f'<circle cx="{x}" cy="{y}" r="5" fill="{t["screen"]}" stroke="{t["audio"]}" stroke-width="2"/>'
    return f'<circle cx="{x}" cy="{y}" r="5.5" fill="{t[kind]}"/>'


def rows(t, rows_, screen):
    sx, _, sw, _ = screen
    node_x, title_x = sx + 28, sx + 52
    max_width = sx + sw - 20 - title_x
    video = [top for kind, _, _, top in rows_ if kind == "video"]
    out = [
        f'<line x1="{node_x}" y1="{video[0] + NODE}" x2="{node_x}" y2="{video[-1] + NODE}" '
        f'stroke="{t["video_rail"]}" stroke-width="1.5"/>'
    ]
    for y in DIVIDERS:
        out.append(f'<line x1="{sx + 24}" y1="{y}" x2="{sx + sw - 24}" y2="{y}" stroke="{t["hairline"]}"/>')
    for kind, title, desc, top in rows_:
        out.append(node(t, kind, node_x, top + NODE))
        out.append(text(MEDIUM, TITLE, title, title_x, top + 18, t["text"], max_width=max_width))
        out.append(text(REGULAR, SMALL, desc, title_x, top + 42, t["muted"], max_width=max_width))
    return out


def monitor(t):
    x, y, w, h = PC["x"], PC["y"], PC["w"], PC["h"]
    sx, sy, sw, sh = PC["screen"]
    cx = x + w / 2
    neck_top, neck_bottom = y + h, y + h + 34
    return [
        f'<path d="M{cx - 26} {neck_top} L{cx + 26} {neck_top} L{cx + 34} {neck_bottom} '
        f'L{cx - 34} {neck_bottom} Z" fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{cx - 76}" y="{neck_bottom}" width="152" height="9" rx="4.5" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{x + 0.5}" y="{y + 0.5}" width="{w - 1}" height="{h - 1}" rx="16" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="{sh}" rx="6" '
        f'fill="{t["screen"]}" stroke="{t["screen_stroke"]}"/>',
        f'<circle cx="{cx}" cy="{sy + sh + (y + h - sy - sh) / 2}" r="2.5" fill="{t["frame_stroke"]}"/>',
    ]


def ipad(t):
    x, y, w, h = IPAD["x"], IPAD["y"], IPAD["w"], IPAD["h"]
    sx, sy, sw, sh = IPAD["screen"]
    return [
        f'<rect x="{x + 0.5}" y="{y + 0.5}" width="{w - 1}" height="{h - 1}" rx="30" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="{sh}" rx="16" '
        f'fill="{t["screen"]}" stroke="{t["screen_stroke"]}"/>',
        f'<circle cx="{x + w / 2}" cy="{y + 7}" r="2.5" fill="{t["frame_stroke"]}"/>',
    ]


def port(t, x, y, color):
    return f'<circle cx="{x}" cy="{y}" r="4" fill="{t["port"]}" stroke="{color}" stroke-width="1.5"/>'


def arrow(t, y, color, to_ipad, width=2, dash=None):
    """A lane from one device edge to the other, with a port where it leaves."""
    start, end = (LEFT, RIGHT) if to_ipad else (RIGHT, LEFT)
    tip = end - 6 if to_ipad else end + 6
    back = tip - 7 if to_ipad else tip + 7
    dashes = f' stroke-dasharray="{dash}"' if dash else ""
    line = f'stroke="{color}" stroke-width="{width}" stroke-linecap="round"'
    return [
        f'<line x1="{start}" y1="{y}" x2="{tip}" y2="{y}" {line}{dashes}/>',
        f'<path d="M{back} {y - 6} L{tip} {y} L{back} {y + 6}" fill="none" {line} stroke-linejoin="round"/>',
        port(t, start, y, color),
    ]


def pill(label, cy, fill, stroke, color):
    w = MEDIUM.width(label, SMALL) + 32
    return [
        f'<rect x="{MID - w / 2:.1f}" y="{cy - 16}" width="{w:.1f}" height="32" rx="16" '
        f'fill="{fill}" stroke="{stroke}"/>',
        text(MEDIUM, SMALL, label, MID, cy + 6, color, "middle"),
    ]


def lanes(t):
    video_y, repair_y = VIDEO_ROWS[1] + NODE, VIDEO_ROWS[2] + NODE + 6
    audio_y, input_y = AUDIO_ROW + NODE, INPUT_ROW + NODE
    out = arrow(t, video_y, t["video"], True, dash="10 7")
    out += pill("Video", video_y, t["video_pill"], t["video_pill_stroke"], t["video_pill_text"])
    out += arrow(t, repair_y, t["input"], False, width=1.5, dash="3 4")
    out.append(text(REGULAR, SMALL, "Repair requests", MID, repair_y - 12, t["muted"], "middle"))
    out.append(text(MONO, SMALL, "Wi-Fi only", MID, repair_y + 26, t["faint"], "middle"))
    out += arrow(t, audio_y, t["audio"], True, width=1.5, dash="4 5")
    out += pill("Audio", audio_y, t["audio_pill"], t["audio_pill_stroke"], t["audio_pill_text"])
    out += arrow(t, input_y, t["input"], False, width=1.5)
    out += pill("Input", input_y, t["chip"], t["chip_stroke"], t["chip_text"])
    return out


def transport(t, cy):
    label = "Wi-Fi or USB"
    w = MEDIUM.width(label, SMALL) + 20 + 46
    left = MID - w / 2
    wx, ux = left + 16, left + 36  # Wi-Fi and USB-C icon centres
    stroke = f'fill="none" stroke="{t["muted"]}" stroke-width="1.4" stroke-linecap="round"'
    return [
        f'<rect x="{left:.1f}" y="{cy - 18}" width="{w:.1f}" height="36" rx="10" '
        f'fill="{t["chip"]}" stroke="{t["chip_stroke"]}" stroke-dasharray="3 3"/>',
        f'<path d="M{wx - 7} {cy - 1.5} A10 10 0 0 1 {wx + 7} {cy - 1.5}" {stroke}/>',
        f'<path d="M{wx - 4} {cy + 1.5} A5.5 5.5 0 0 1 {wx + 4} {cy + 1.5}" {stroke}/>',
        f'<circle cx="{wx}" cy="{cy + 4.5}" r="1.4" fill="{t["muted"]}"/>',
        f'<rect x="{ux - 7}" y="{cy - 3.5}" width="14" height="7" rx="3.5" {stroke}/>',
        f'<line x1="{ux - 3}" y1="{cy}" x2="{ux + 3}" y2="{cy}" {stroke}/>',
        text(MEDIUM, SMALL, label, left + 52, cy + 6, t["chip_text"]),
    ]


def headers(t):
    out = []
    for x, name, detail in ((PC["x"], "Windows PC", "Rust host"), (IPAD["x"], "iPad", "Swift app")):
        out.append(text(SEMIBOLD, HEADING, name, x + 2, 64, t["text"]))
        out.append(text(MONO, SMALL, detail, x + 2 + SEMIBOLD.width(name, HEADING) + 12, 64, t["faint"]))
    return out


def footer(t):
    y = 656
    return [
        f'<line x1="40" y1="616" x2="{W - 40}" y2="616" stroke="{t["hairline"]}"/>',
        text(MONO, SMALL, "UDP fragments on Wi-Fi · framed tunnel on USB", 40, y, t["faint"]),
        text(REGULAR, SMALL, "Lost audio is concealed, not repaired", W - 40, y, t["muted"], "end"),
    ]


ALT = (
    "The Windows PC captures the desktop, encodes the video on the GPU and sends it "
    "over Wi-Fi or USB. The iPad reassembles the video, asking for missing fragments "
    "on Wi-Fi, decodes it with VideoToolbox and shows it with Metal. PC audio is "
    "captured with WASAPI, encoded as Opus and played on the iPad. Touch, Pencil, "
    "keyboard and trackpad input travels back to the PC, which injects it."
)


def draw(name, t):
    body = [
        f'<rect x="0.5" y="0.5" width="{W - 1}" height="{H - 1}" rx="24" fill="{t["bg"]}" '
        f'stroke="{t["card_border"]}"/>',
        f'<rect width="{W}" height="{H}" rx="24" fill="url(#dots)" mask="url(#fade)"/>',
        f'<ellipse cx="{MID}" cy="300" rx="260" ry="230" fill="url(#glow)"/>',
        *headers(t),
        *monitor(t),
        *ipad(t),
        *rows(t, PC_ROWS, PC["screen"]),
        *rows(t, IPAD_ROWS, IPAD["screen"]),
        *lanes(t),
        *transport(t, 150),
        *footer(t),
    ]
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
        f'viewBox="0 0 {W} {H}" role="img" aria-labelledby="title desc">\n'
        f'<title id="title">How EternalMonitor works</title>\n'
        f'<desc id="desc">{ALT}</desc>\n'
        "<defs>\n"
        '<pattern id="dots" width="24" height="24" patternUnits="userSpaceOnUse">'
        f'<circle cx="12" cy="12" r="1" fill="{t["dots"]}"/></pattern>\n'
        '<radialGradient id="fadeGrad" cx="50%" cy="42%" r="62%">'
        '<stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#fff" stop-opacity="0"/>'
        "</radialGradient>\n"
        f'<mask id="fade"><rect width="{W}" height="{H}" fill="url(#fadeGrad)"/></mask>\n'
        '<radialGradient id="glow">'
        f'<stop offset="0" stop-color="{t["glow"]}" stop-opacity="{t["glow_opacity"]}"/>'
        f'<stop offset="1" stop-color="{t["glow"]}" stop-opacity="0"/></radialGradient>\n'
        "</defs>\n" + "\n".join(body) + "\n</svg>\n"
    )
    path = OUT / f"how-it-works-{name}.svg"
    path.write_text(svg)
    print(f"{path.relative_to(ROOT)}: {len(svg.encode()) / 1024:.0f} KB")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for theme, tokens in THEMES.items():
        draw(theme, tokens)
