# Builds the home page hero's pictures from their sources:
#
#   python3 site-demos/capture/hero_compose.py
#
# Needs Pillow and NumPy (pip install pillow numpy) and cwebp. Reads
#   art/hero-laptop.webp            the laptop, its display solid blue
#   renders/hero/ipad.png, cable.png  hero_scene.py's layers at 2x the canvas
#   footage/hero-laptop-*.png, hero-ipad.png  capture-doc.sh's screens
# and writes docs/assets/hero/, each picture at 1x and 2x:
#   devices    the laptop and the iPad on its dock, both displays cut out
#   cable      the USB-C cable and plugs, cropped to themselves
#   laptop-<mode>-<link>  the laptop's screen, sized to its display
#   ipad-second  the iPad's own desktop, sized to its display
# It prints where the displays and the cable sit on the canvas, in percent,
# for docs/style.css.
import json
import pathlib
import subprocess
import tempfile

import numpy as np
from PIL import Image

HERE = pathlib.Path(__file__).resolve().parent
DEMOS = HERE.parent
OUT = DEMOS.parent / "docs" / "assets" / "hero"
W, H = 2709, 980          # the canvas, which is the 2x picture
LAPTOP_AT = (-138, 4)     # where the laptop render's corner sits on the canvas
BLEED = 3                 # screens run this far under their bezels


def load_layer(path):
    """A 2x render brought down to the canvas, filtered with premultiplied alpha."""
    im = Image.open(path).convert("RGBA").convert("RGBa")
    return im.resize((W, H), Image.LANCZOS).convert("RGBA")


def laptop_layer():
    src = np.array(Image.open(DEMOS / "art" / "hero-laptop.webp").convert("RGBA")).astype(np.float32)
    r, g, b, a = (src[..., i] for i in range(4))
    blue = np.clip((b - np.maximum(r, g) - 60) / 80, 0, 1)
    # Edge pixels keep the bezel's black, without the blue that bled into them.
    src[..., 2] = np.where(blue > 0, np.minimum(b, np.maximum(r, g)), b)
    src[..., 3] = a * (1 - blue)
    shell = Image.fromarray(src.clip(0, 255).astype(np.uint8))
    canvas = Image.new("RGBA", (W, H))
    canvas.paste(shell, LAPTOP_AT)
    ys, xs = np.nonzero(blue > 0.5)
    hole = (xs.min() + LAPTOP_AT[0], ys.min() + LAPTOP_AT[1], xs.max() + 1 + LAPTOP_AT[0], ys.max() + 1 + LAPTOP_AT[1])
    return canvas, hole


def ipad_hole(layer):
    """The display: the transparent region in the middle of the iPad's body."""
    a = np.array(layer)[..., 3]
    ys, xs = np.nonzero(a > 128)
    body = xs[xs > 1500]
    cx = int((body.min() + body.max()) / 2)
    col = np.nonzero(a[:, cx] < 128)[0]
    top_bezel = np.nonzero(a[:, cx] > 128)[0].min()
    inside = col[col > top_bezel + 5]
    top = inside.min()
    bottom = top + np.argmax(np.diff(inside) > 1) if np.any(np.diff(inside) > 1) else inside.max()
    cy = int((top + bottom) / 2)
    row = np.nonzero(a[cy] < 128)[0]
    row = row[(row > body.min()) & (row < body.max())]
    return (int(row.min()), int(top), int(row.max()) + 1, int(bottom) + 1)


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
                args += ["-alpha_q", "90", "-exact"]
            subprocess.run(args + [tmp.name, "-o", str(target)], check=True)
            sizes[target.name] = target.stat().st_size
    return sizes


def pct(box):
    x0, y0, x1, y1 = box
    return {"left": round(100 * x0 / W, 3), "top": round(100 * y0 / H, 3),
            "width": round(100 * (x1 - x0) / W, 3), "height": round(100 * (y1 - y0) / H, 3)}


def grow(box, n):
    return (box[0] - n, box[1] - n, box[2] + n, box[3] + n)


laptop, laptop_hole = laptop_layer()
ipad = load_layer(DEMOS / "renders" / "hero" / "ipad.png")
ipad_display = ipad_hole(ipad)
devices = laptop.copy()
devices.alpha_composite(ipad)

# The cable's plug leaves the laptop's side, which the laptop hides; cut the
# cable where the laptop covers it near the port.
cable = np.array(load_layer(DEMOS / "renders" / "hero" / "cable.png")).astype(np.float32)
cover = np.array(laptop)[..., 3].astype(np.float32) / 255
near_port = np.zeros((H, W), bool)
near_port[770:880, 1260:1380] = True
cable[..., 3] *= np.where(near_port, 1 - cover, 1)
cable[..., 3] *= 1 - np.array(ipad)[..., 3].astype(np.float32) / 255
cable[:, 1790:, 3] = 0  # the plug ends at the iPad's edge, x = 1760
# The renderer's faint noise far from the cable would stretch its crop.
cable[..., 3] = np.where(cable[..., 3] < 4, 0, cable[..., 3])
cable_im = Image.fromarray(cable.clip(0, 255).astype(np.uint8))
cable_box = cable_im.getbbox()
cable_box = (cable_box[0] - 2, cable_box[1] - 2, cable_box[2] + 2, cable_box[3] + 2)

sizes = {}
sizes.update(webp(devices, "devices", alpha=True))
sizes.update(webp(cable_im.crop(cable_box), "cable", alpha=True))

laptop_box = grow(laptop_hole, BLEED)
lw, lh = laptop_box[2] - laptop_box[0], laptop_box[3] - laptop_box[1]
for mode in ("mirror", "second"):
    for link in ("usb", "wifi"):
        shot = Image.open(DEMOS / "footage" / f"hero-laptop-{mode}-{link}.png").convert("RGB")
        sizes.update(webp(shot.resize((lw, lh), Image.LANCZOS), f"laptop-{mode}-{link}", quality=82))
ipad_box = grow(ipad_display, BLEED)
iw, ih = ipad_box[2] - ipad_box[0], ipad_box[3] - ipad_box[1]
shot = Image.open(DEMOS / "footage" / "hero-ipad.png").convert("RGB")
sizes.update(webp(shot.resize((iw, ih), Image.LANCZOS), "ipad-second", quality=82))

print(json.dumps({
    "canvas": [W, H],
    "laptop_screen": pct(laptop_box),
    "ipad_screen": pct(ipad_box),
    "ipad_screen_px": ipad_box,
    "cable": pct(cable_box),
    "cable_px": cable_box,
    "bytes": sizes,
}, indent=1))
