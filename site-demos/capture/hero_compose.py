# Builds the home page hero's pictures from their sources:
#
#   python3 site-demos/capture/hero_compose.py
#
# Needs Pillow and NumPy (pip install pillow numpy) and cwebp. Reads
#   art/hero-laptop.webp            the laptop, its display solid blue
#   renders/hero/<layout>/          hero_scene.py's layers at 2x the canvas,
#     for the wide layout and the narrow one phones get
#   footage/hero-laptop-*.png, hero-ipad.png  capture-doc.sh's screens
# and writes docs/assets/hero/ (or $HERO_OUT), each picture at 1x and 2x:
#   devices, devices-narrow  the desk's shadow, the laptop and the iPad on its
#              dock, both displays cut out
#   cable, cable-narrow  the USB-C cable, its plugs and its shadow, cropped to
#              themselves
#   laptop-<mode>-<link>  the laptop's screen, sized to its display
#   ipad-second  the iPad's own desktop, sized to its display
# It prints where the displays and the cable sit on each canvas, in percent,
# for docs/style.css.
import json
import os
import pathlib
import subprocess
import tempfile

import numpy as np
from PIL import Image, ImageFilter

HERE = pathlib.Path(__file__).resolve().parent
DEMOS = HERE.parent
OUT = pathlib.Path(os.environ.get("HERO_OUT", DEMOS.parent / "docs" / "assets" / "hero"))
# The canvases (each the 2x picture) and where the laptop render's corner
# sits on them; hero_scene.py renders its layers for the same numbers.
LAYOUTS = {
    "wide": dict(W=2709, H=980, laptop_at=(-138, 4), suffix=""),
    "narrow": dict(W=2240, H=1200, laptop_at=(-140, 4), suffix="-narrow"),
}
BLEED = 3                 # screens run this far under their bezels
EDGE = 48                 # shadows fade out over this many pixels at the canvas's edges
REACH = 160               # the cable's shadow is kept this near the cable


def load_layer(path, size):
    """A 2x render brought down to the canvas, filtered with premultiplied alpha."""
    im = Image.open(path).convert("RGBA").convert("RGBa")
    return np.array(im.resize(size, Image.LANCZOS).convert("RGBA")).astype(np.float32) / 255


def laptop_layer(W, H, at):
    src = np.array(Image.open(DEMOS / "art" / "hero-laptop.webp").convert("RGBA")).astype(np.float32)
    r, g, b, a = (src[..., i] for i in range(4))
    blue = np.clip((b - np.maximum(r, g) - 60) / 80, 0, 1)
    # Edge pixels keep the bezel's black, without the blue that bled into them.
    src[..., 2] = np.where(blue > 0, np.minimum(b, np.maximum(r, g)), b)
    src[..., 3] = a * (1 - blue)
    shell = Image.fromarray(src.clip(0, 255).astype(np.uint8))
    canvas = Image.new("RGBA", (W, H))
    canvas.paste(shell, at)
    ys, xs = np.nonzero(blue > 0.5)
    hole = (xs.min() + at[0], ys.min() + at[1], xs.max() + 1 + at[0], ys.max() + 1 + at[1])
    return np.array(canvas).astype(np.float32) / 255, tuple(int(v) for v in hole)


def ipad_hole(a):
    """The display: the transparent region in the middle of the iPad's body."""
    ys, xs = np.nonzero(a > 0.5)
    cx = int((xs.min() + xs.max()) / 2)
    col = np.nonzero(a[:, cx] < 0.5)[0]
    top_bezel = np.nonzero(a[:, cx] > 0.5)[0].min()
    inside = col[col > top_bezel + 5]
    top = inside.min()
    bottom = top + np.argmax(np.diff(inside) > 1) if np.any(np.diff(inside) > 1) else inside.max()
    cy = int((top + bottom) / 2)
    row = np.nonzero(a[cy] < 0.5)[0]
    row = row[(row > xs.min()) & (row < xs.max())]
    return (int(row.min()), int(top), int(row.max()) + 1, int(bottom) + 1)


def over(top, bottom):
    """Premultiplied 'top over bottom' for straight-alpha float arrays."""
    ta, ba = top[..., 3:], bottom[..., 3:]
    a = ta + ba * (1 - ta)
    rgb = top[..., :3] * ta + bottom[..., :3] * ba * (1 - ta)
    return np.concatenate([np.where(a > 0, rgb / np.maximum(a, 1e-6), 0), a], axis=-1)


def to_image(arr):
    return Image.fromarray((arr.clip(0, 1) * 255 + 0.5).astype(np.uint8))


def webp(im, name, quality=84, alpha=False):
    """name.webp at 1x and name@2x.webp, from a 2x picture."""
    OUT.mkdir(parents=True, exist_ok=True)
    half = im.convert("RGBa").resize((round(im.width / 2), round(im.height / 2)), Image.LANCZOS).convert("RGBA")
    sizes = {}
    for suffix, pic in (("", half), ("@2x", im)):
        with tempfile.NamedTemporaryFile(suffix=".png") as tmp:
            (pic if alpha else pic.convert("RGB")).save(tmp.name)
            target = OUT / f"{name}{suffix}.webp"
            args = ["cwebp", "-quiet", "-q", str(quality), "-m", "6", "-sharp_yuv"]
            if alpha:
                # Lossy alpha steps the shadows' falloff, but by less than one
                # level against the page's near-black, where they never show.
                args += ["-alpha_q", "70", "-exact"]
            subprocess.run(args + [tmp.name, "-o", str(target)], check=True)
            sizes[target.name] = target.stat().st_size
    return sizes


def fit(shot, w, h):
    """The screen picture scaled to cover w x h and cropped to it, never stretched."""
    s = max(w / shot.width, h / shot.height)
    big = shot.resize((round(shot.width * s), round(shot.height * s)), Image.LANCZOS)
    x, y = (big.width - w) // 2, (big.height - h) // 2
    return big.crop((x, y, x + w, y + h))


def near(alpha, reach):
    """1 within about `reach` pixels of where alpha is set, falling to 0 past it."""
    small = Image.fromarray((alpha * 255).astype(np.uint8)).reduce(8)
    small = small.filter(ImageFilter.MaxFilter(2 * (reach // 16) + 1)).filter(ImageFilter.GaussianBlur(reach / 16))
    return np.array(small.resize((alpha.shape[1], alpha.shape[0]), Image.BILINEAR)).astype(np.float32) / 255


def grow(box, n):
    return (box[0] - n, box[1] - n, box[2] + n, box[3] + n)


def compose(layout):
    L = LAYOUTS[layout]
    W, H, suffix = L["W"], L["H"], L["suffix"]
    pct = lambda box: {"left": round(100 * box[0] / W, 3), "top": round(100 * box[1] / H, 3),
                       "width": round(100 * (box[2] - box[0]) / W, 3), "height": round(100 * (box[3] - box[1]) / H, 3)}
    renders = DEMOS / "renders" / "hero" / layout
    laptop, laptop_hole = laptop_layer(W, H, L["laptop_at"])
    ipad = load_layer(renders / "ipad.png", (W, H))
    shadow = load_layer(renders / "shadow.png", (W, H))
    cable = load_layer(renders / "cable.png", (W, H))
    cable_shadow = load_layer(renders / "cable-shadow.png", (W, H))
    ipad_display = ipad_hole(ipad[..., 3])
    laptop_box, ipad_box = grow(laptop_hole, BLEED), grow(ipad_display, BLEED)

    # The shadows fade out before the canvas's edges, and never cover a display.
    yy, xx = np.mgrid[0:H, 0:W]
    edge = np.minimum.reduce([xx, W - 1 - xx, yy, H - 1 - yy]).astype(np.float32)
    fade = np.clip(edge / EDGE, 0, 1)
    open_ = np.ones((H, W), np.float32)
    for x0, y0, x1, y1 in (laptop_box, ipad_box):
        open_[y0:y1, x0:x1] = 0
    shadow[..., 3] *= fade * open_
    shadow[..., 3] = np.where(shadow[..., 3] < 3 / 255, 0, shadow[..., 3])
    # Where the iPad stands in front of the laptop, its display shows the
    # screen beneath, not the laptop behind it.
    x0, y0, x1, y1 = ipad_box
    laptop[y0:y1, x0:x1, 3] = 0
    devices = over(ipad, over(laptop, shadow))

    # The cable's shadow, then the cable. The holdouts in the render already cut
    # the cable where the laptop and the iPad stand in front of it.
    covered = 1 - (1 - laptop[..., 3]) * (1 - ipad[..., 3])
    cable_shadow[..., 3] *= fade * open_ * (1 - covered) * np.clip(2 * near(cable[..., 3], REACH), 0, 1)
    cable[..., 3] *= 1 - ipad[..., 3]
    wire = over(cable, cable_shadow)
    # The renderer's faint noise far from the cable would stretch its crop.
    wire[..., 3] = np.where(wire[..., 3] < 4 / 255, 0, wire[..., 3])
    wire_im = to_image(wire)
    cable_box = grow(wire_im.getbbox(), 2)

    sizes = {}
    sizes.update(webp(to_image(devices), "devices" + suffix, alpha=True))
    sizes.update(webp(wire_im.crop(cable_box), "cable" + suffix, alpha=True))
    return {
        "canvas": [W, H],
        "laptop_screen": pct(laptop_box),
        "laptop_screen_px": laptop_box,
        "ipad_screen": pct(ipad_box),
        "ipad_screen_px": ipad_box,
        "cable": pct(cable_box),
        "cable_px": cable_box,
        "bytes": sizes,
    }


report = {layout: compose(layout) for layout in LAYOUTS}

# The screens, sized to the wide layout's displays; the narrow one shows the
# same pictures, scaled.
wide = report["wide"]
lx0, ly0, lx1, ly1 = wide["laptop_screen_px"]
ix0, iy0, ix1, iy1 = wide["ipad_screen_px"]
sizes = {}
for mode in ("mirror", "second"):
    for link in ("usb", "wifi"):
        shot = Image.open(DEMOS / "footage" / f"hero-laptop-{mode}-{link}.png").convert("RGB")
        sizes.update(webp(fit(shot, lx1 - lx0, ly1 - ly0), f"laptop-{mode}-{link}", quality=82))
shot = Image.open(DEMOS / "footage" / "hero-ipad.png").convert("RGB")
sizes.update(webp(fit(shot, ix1 - ix0, iy1 - iy0), "ipad-second", quality=82))
report["screens"] = {"laptop_px": [lx1 - lx0, ly1 - ly0], "ipad_px": [ix1 - ix0, iy1 - iy0], "bytes": sizes}
print(json.dumps(report, indent=1))
