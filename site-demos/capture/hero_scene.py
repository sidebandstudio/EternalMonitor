# The home page hero's iPad, its dock and the USB-C cable, rendered in Blender
# to sit beside the laptop render (site-demos/art/hero-laptop.webp).
#
#   blender -b -P site-demos/capture/hero_scene.py -- <out folder> [scale] [layout] [--check] [--draft]
#
# layout is "wide" (the default, the hero's canvas on wider screens) or
# "narrow" (phones: the iPad moved in to overlap the laptop's front corner).
# Writes, each the layout's W x H canvas pixels times scale:
#   ipad.png    the tablet and its dock, the display left transparent
#   shadow.png  the shadow the laptop, the dock and the iPad cast on the desk
#   cable.png   the cable and both plugs, cut where the laptop hides them
#   cable-shadow.png  the shadow the cable alone casts on the desk
# --check prints where the laptop's deck corners land and renders nothing.
#
# Units are millimetres: x across, y away from the camera, z up from the desk;
# the laptop's centre line is x = 0 and its front edge y = 0. The camera is
# the laptop render's own, solved from its deck: the front edge (row 886, x
# 159 to 1513 in the laptop picture) and the hinge (row 668, x 285 to 1387)
# of a 310 x 215 mm deck, with the principal point in the picture's middle,
# give a focal length of 4036 picture pixels, a 10.6 degree downward pitch,
# and an eye 264 mm above the deck and 891 mm in front of it. The iPad leans
# back by the same angle, so its display faces the lens squarely and stays a
# rectangle the page can lay a picture into.
import math
import sys

import bpy
import bmesh
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Matrix, Vector

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
args = [a for a in argv if not a.startswith("--")]
OUT = args[0] if args else "/tmp"
SCALE = float(args[1]) if len(args) > 1 else 2.0
LAYOUT = args[2] if len(args) > 2 else "wide"
CHECK = "--check" in argv
DRAFT = "--draft" in argv  # a quick, noisy look

FOCAL = 4036.4           # picture pixels
PITCH = math.radians(10.606)
EYE_Y, EYE_Z_DECK = -890.8, 263.6
PP = (836, 470.5)        # the principal point in the laptop picture
DECK_W, DECK_D = 310.0, 215.0
DECK_TOP = 6.4           # the deck's top above the desk, on its feet
FEET = 1.2

IPAD_W, IPAD_H, IPAD_T, IPAD_R = 249.7, 177.5, 5.3, 12.0
SCREEN_W, SCREEN_H, SCREEN_R = 233.0, 160.5, 7.5
CABLE_R = 1.9

# Per layout: the canvas, where the laptop picture's corner sits on it (as in
# hero_compose.py), and where the iPad stands (its left edge and the front of
# its face, at the desk).
LAYOUTS = {
    "wide": dict(W=2709, H=980, laptop_at=(-138, 4), ipad_x=270.0, ipad_y=140.0),
    "narrow": dict(W=2240, H=1200, laptop_at=(-140, 4), ipad_x=38.0, ipad_y=-150.0),
}
L = LAYOUTS[LAYOUT]
W, H = L["W"], L["H"]
IPAD_LEFT, IPAD_FRONT = L["ipad_x"], L["ipad_y"]


def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    prefs = bpy.context.preferences.addons["cycles"].preferences
    try:
        prefs.compute_device_type = "METAL"
        prefs.get_devices()
        for d in prefs.devices:
            d.use = True
        scene.cycles.device = "GPU"
    except Exception:
        scene.cycles.device = "CPU"
    scene.cycles.samples = 192
    scene.cycles.use_denoising = True
    scene.render.film_transparent = True
    scene.render.resolution_x = round(W * SCALE)
    scene.render.resolution_y = round(H * SCALE)
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA"
    scene.render.image_settings.color_depth = "16"
    scene.view_settings.view_transform = "Standard"
    scene.view_settings.look = "None"
    scene.unit_settings.system = "NONE"
    world = bpy.data.worlds.new("World")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.012, 0.012, 0.014, 1)
    scene.world = world
    return scene


def material(name, color, metallic=0.0, roughness=0.5, coat=0.0, aniso=0.0, emission=None):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    p = m.node_tree.nodes["Principled BSDF"]
    p.inputs["Base Color"].default_value = (*color, 1)
    p.inputs["Metallic"].default_value = metallic
    p.inputs["Roughness"].default_value = roughness
    p.inputs["Coat Weight"].default_value = coat
    p.inputs["Coat Roughness"].default_value = 0.03
    p.inputs["Anisotropic"].default_value = aniso
    if emission:
        p.inputs["Emission Color"].default_value = (*emission, 1)
        p.inputs["Emission Strength"].default_value = 1.0
    return m


def braided(name, color, roughness):
    """The cable's jacket: dark graphite with a fine diagonal weave in its bump."""
    m = material(name, color, roughness=roughness)
    nodes, links = m.node_tree.nodes, m.node_tree.links
    coord = nodes.new("ShaderNodeTexCoord")
    bump = nodes.new("ShaderNodeBump")
    bump.inputs["Strength"].default_value = 0.35
    bump.inputs["Distance"].default_value = 0.15
    mix = nodes.new("ShaderNodeMath")
    mix.operation = "MULTIPLY"
    for i, direction in enumerate(("X", "Y")):
        wave = nodes.new("ShaderNodeTexWave")
        wave.wave_type = "BANDS"
        wave.bands_direction = "DIAGONAL"
        wave.inputs["Scale"].default_value = 1.6
        wave.inputs["Distortion"].default_value = 0.0
        if i:
            flip = nodes.new("ShaderNodeMapping")
            flip.inputs["Rotation"].default_value = (0, 0, math.pi / 2)
            links.new(coord.outputs["Object"], flip.inputs["Vector"])
            links.new(flip.outputs["Vector"], wave.inputs["Vector"])
        else:
            links.new(coord.outputs["Object"], wave.inputs["Vector"])
        links.new(wave.outputs["Fac"], mix.inputs[i])
    links.new(mix.outputs[0], bump.inputs["Height"])
    links.new(bump.outputs["Normal"], nodes["Principled BSDF"].inputs["Normal"])
    return m


def holdout(name):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nodes = m.node_tree.nodes
    nodes.remove(nodes["Principled BSDF"])
    h = nodes.new("ShaderNodeHoldout")
    m.node_tree.links.new(h.outputs[0], nodes["Material Output"].inputs["Surface"])
    return m


def rounded_rect(w, h, r, seg=24):
    """Outline of a w x h rectangle with corner radius r, in the XZ plane, from (0, 0)."""
    pts = []
    for cx, cz, a0 in ((w - r, r, -90), (w - r, h - r, 0), (r, h - r, 90), (r, r, 180)):
        for i in range(seg + 1):
            a = math.radians(a0 + 90 * i / seg)
            pts.append((cx + r * math.cos(a), cz + r * math.sin(a)))
    return pts


def slab(name, pts, y0, y1, mat, bevel=0.0, collection=None):
    """A flat shape extruded from y0 to y1 (toward +y, away from the camera)."""
    mesh = bpy.data.meshes.new(name)
    bm = bmesh.new()
    front = [bm.verts.new((x, y0, z)) for x, z in pts]
    back = [bm.verts.new((x, y1, z)) for x, z in pts]
    bm.faces.new(list(reversed(front)))
    bm.faces.new(back)
    n = len(pts)
    for i in range(n):
        j = (i + 1) % n
        bm.faces.new((front[i], front[j], back[j], back[i]))
    bm.normal_update()
    bm.to_mesh(mesh)
    bm.free()
    obj = bpy.data.objects.new(name, mesh)
    (collection or bpy.context.scene.collection).objects.link(obj)
    obj.data.materials.append(mat)
    if bevel:
        mod = obj.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 6
        mod.limit_method = "ANGLE"
        mod.angle_limit = math.radians(40)
        mod.harden_normals = True
    for poly in obj.data.polygons:
        poly.use_smooth = True
    return obj


def face(name, pts, y, mat, collection):
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata([(x, y, z) for x, z in pts], [], [list(range(len(pts)))[::-1]])
    obj = bpy.data.objects.new(name, mesh)
    collection.objects.link(obj)
    obj.data.materials.append(mat)
    return obj


def box(name, size, mat, collection, bevel=0.0):
    """A rounded box centred on the origin."""
    sx, sy, sz = size
    mesh = bpy.data.meshes.new(name)
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=1.0)
    bmesh.ops.scale(bm, vec=(sx, sy, sz), verts=bm.verts)
    bm.to_mesh(mesh)
    bm.free()
    obj = bpy.data.objects.new(name, mesh)
    collection.objects.link(obj)
    obj.data.materials.append(mat)
    if bevel:
        mod = obj.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 8
        mod.harden_normals = True
    for poly in obj.data.polygons:
        poly.use_smooth = True
    return obj


def tube(name, points, radius, mat, collection, r_end=None):
    curve = bpy.data.curves.new(name, "CURVE")
    curve.dimensions = "3D"
    curve.resolution_u = 64
    curve.bevel_depth = radius
    curve.bevel_resolution = 8
    curve.use_fill_caps = True
    spline = curve.splines.new("NURBS")
    spline.points.add(len(points) - 1)
    for p, co in zip(spline.points, points):
        p.co = (*co, 1)
    spline.use_endpoint_u = True
    spline.order_u = 4
    if r_end is not None:
        n = len(points)
        for i, p in enumerate(spline.points):
            p.radius = 1 + (r_end / radius - 1) * i / (n - 1)
    obj = bpy.data.objects.new(name, curve)
    collection.objects.link(obj)
    obj.data.materials.append(mat)
    return obj


def plug(name, frame, cable_mats, collection):
    """A USB-C plug in `frame` (a world matrix whose +x points out of the port):
    a bare steel collar at the port, a short soft-touch graphite boot, then a
    tapered strain relief. Returns the relief's far end."""
    boot_mat, relief_mat, metal_mat = cable_mats
    parts = []
    shell = box(name + "-shell", (1.2, 8.2, 2.5), metal_mat, collection, bevel=0.5)
    shell.location = (0.4, 0, 0)
    parts.append(shell)
    collar = box(name + "-collar", (1.6, 10.0, 4.4), metal_mat, collection, bevel=0.7)
    collar.location = (1.6, 0, 0)
    parts.append(collar)
    boot = box(name + "-boot", (12.0, 10.4, 4.8), boot_mat, collection, bevel=2.0)
    boot.location = (8.2, 0, 0)
    parts.append(boot)
    relief = tube(name + "-relief", [(13.6, 0, 0), (16.0, 0, 0), (18.5, 0, 0), (21.0, 0, 0)], 2.7, relief_mat, collection, r_end=CABLE_R + 0.1)
    parts.append(relief)
    for p in parts:
        p.matrix_world = frame @ p.matrix_basis
    return frame @ Vector((21.0, 0, 0))


def light(name, kind, loc, target, energy, size, color=(1, 1, 1)):
    data = bpy.data.lights.new(name, kind)
    data.energy = energy
    data.color = color
    if kind == "AREA":
        data.shape = "RECTANGLE"
        data.size = size[0]
        data.size_y = size[1]
    obj = bpy.data.objects.new(name, data)
    bpy.context.scene.collection.objects.link(obj)
    obj.location = loc
    direction = Vector(target) - Vector(loc)
    obj.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()
    return obj


scene = reset()
col_ipad = bpy.data.collections.new("ipad")
col_cable = bpy.data.collections.new("cable")
col_laptop = bpy.data.collections.new("laptop")
col_desk = bpy.data.collections.new("desk")
for c in (col_ipad, col_cable, col_laptop, col_desk):
    scene.collection.children.link(c)

alu = material("alu", (0.55, 0.55, 0.57), metallic=1.0, roughness=0.36, aniso=0.2)
alu_dock = material("alu-dock", (0.17, 0.17, 0.18), metallic=1.0, roughness=0.42, aniso=0.2)
slot = material("slot", (0.01, 0.01, 0.01), roughness=0.8)
glass = material("glass", (0.004, 0.004, 0.005), roughness=0.04, coat=1.0)
camera_dot = material("camera-dot", (0.015, 0.018, 0.03), roughness=0.1, coat=1.0)
screen = holdout("screen")
jacket = braided("jacket", (0.045, 0.045, 0.05), roughness=0.48)
boot_mat = material("boot", (0.07, 0.07, 0.075), roughness=0.45, coat=0.05)
relief_mat = material("relief", (0.05, 0.05, 0.055), roughness=0.55)
steel = material("steel", (0.78, 0.78, 0.8), metallic=1.0, roughness=0.18)
graphite = material("graphite", (0.06, 0.06, 0.065), metallic=1.0, roughness=0.45)
desk_mat = material("desk", (0.03, 0.03, 0.03), roughness=0.7)

# The laptop, as a stand-in: the camera never sees it (the laptop picture
# does that), but it shades the desk, the cable and the iPad beside it.
base = box("laptop-base", (DECK_W, DECK_D, DECK_TOP - FEET), graphite, col_laptop, bevel=2.0)
base.location = (0, DECK_D / 2, FEET + (DECK_TOP - FEET) / 2)
for fx in (-120, 120):
    for fy in (25, DECK_D - 25):
        foot = box("laptop-foot", (30, 8, FEET), graphite, col_laptop)
        foot.location = (fx, fy, FEET / 2)
lid = box("laptop-lid", (DECK_W, 4.5, 182.0), graphite, col_laptop, bevel=1.5)
lid.matrix_world = (Matrix.Translation((0, DECK_D - 3, DECK_TOP)) @ Matrix.Rotation(-PITCH, 4, "X")
                    @ Matrix.Translation((0, 2.25, 91.0)))

# The desk: it only catches shadows.
desk = bpy.data.objects.new("desk", bpy.data.meshes.new("desk"))
desk.data.from_pydata([(-3000, -1500, 0), (4000, -1500, 0), (4000, 3000, 0), (-3000, 3000, 0)], [], [(0, 1, 2, 3)])
desk.data.materials.append(desk_mat)
col_desk.objects.link(desk)
desk.is_shadow_catcher = True

# The iPad, built in its own frame: x across, z up, front face at y = 0,
# leaning back in the dock's slot.
dock_w, dock_d, dock_h = 150.0, 52.0, 15.0
IPAD_LIFT = dock_h - 6.0
ipad_frame = Matrix.Translation((IPAD_LEFT, IPAD_FRONT, IPAD_LIFT)) @ Matrix.Rotation(-PITCH, 4, "X")
body = slab("body", rounded_rect(IPAD_W, IPAD_H, IPAD_R), 0.0, IPAD_T, alu, bevel=0.9, collection=col_ipad)
glass_face = face("glass", [(x, z) for x, z in rounded_rect(IPAD_W - 1.0, IPAD_H - 1.0, IPAD_R - 0.5)], -0.02, glass, col_ipad)
glass_face.location = (0.5, 0, 0.5)
bx, bz = (IPAD_W - SCREEN_W) / 2, (IPAD_H - SCREEN_H) / 2
display = face("display", rounded_rect(SCREEN_W, SCREEN_H, SCREEN_R), -0.04, screen, col_ipad)
display.location = (bx, 0, bz)
dot = face("camera", rounded_rect(2.2, 2.2, 1.1, 8), -0.05, camera_dot, col_ipad)
dot.location = (IPAD_W / 2 - 1.1, 0, IPAD_H - bz / 2 - 1.1)
for obj in (body, glass_face, display, dot):
    obj.matrix_world = ipad_frame @ obj.matrix_basis

# The dock: a low graphite aluminium bar, the laptop's finish, with a slot
# the iPad's bottom edge rests in.
dock = box("dock", (dock_w, dock_d, dock_h), alu_dock, col_ipad, bevel=2.4)
dock.location = (IPAD_LEFT + IPAD_W / 2, IPAD_FRONT + 12.0, dock_h / 2)
cutter = box("dock-slot", (dock_w - 24, IPAD_T + 1.4, 30), slot, col_ipad)
cutter.matrix_world = ipad_frame @ Matrix.Translation((IPAD_W / 2, IPAD_T / 2, 0))
cut = dock.modifiers.new("slot", "BOOLEAN")
cut.operation = "DIFFERENCE"
cut.object = cutter
cutter.hide_render = True
cutter.hide_viewport = True
slot_floor = box("dock-slot-floor", (dock_w - 24, IPAD_T + 1.4, 0.6), slot, col_ipad)
slot_floor.matrix_world = ipad_frame @ Matrix.Translation((IPAD_W / 2, IPAD_T / 2, -0.3))

# Plugs. The laptop's port is in its right side, mid-way up its base; the
# iPad's in the middle of its left edge.
laptop_port = Vector((DECK_W / 2 - 1.0, 62.0, FEET + (DECK_TOP - FEET) / 2))
start = plug("laptop-plug", Matrix.Translation(laptop_port), (boot_mat, relief_mat, steel), col_cable)
ipad_port = ipad_frame @ Vector((0, IPAD_T / 2, IPAD_H / 2))
# Out of the iPad's left edge: local -x, with the plug's flat side facing the camera.
# The plug's axes: x out of the port, y across its wide side, z across its thin side.
out_of_left_edge = Matrix(((-1, 0, 0), (0, 0, 1), (0, 1, 0))).to_4x4()
ipad_plug_frame = Matrix.Translation(ipad_port) @ ipad_frame.to_3x3().to_4x4() @ out_of_left_edge
end = plug("ipad-plug", ipad_plug_frame, (boot_mat, relief_mat, steel), col_cable)

r = CABLE_R
if LAYOUT == "wide":
    # Out of the laptop, down to the desk, a loose loop toward the camera,
    # then up beside the dock and round into the iPad's port.
    gap = end.x - start.x
    mid_y = min(start.y, IPAD_FRONT) - 55
    path = [
        start,
        start + Vector((8, -1, -1.5)),
        Vector((start.x + 0.16 * gap, start.y - 6, r + 0.6)),
        Vector((start.x + 0.30 * gap, mid_y + 25, r)),
        Vector((start.x + 0.50 * gap, mid_y, r)),
        Vector((start.x + 0.72 * gap, mid_y + 6, r)),
        Vector((end.x - 18, IPAD_FRONT - 12, r)),
        Vector((end.x - 24, IPAD_FRONT - 4, 8)),
        Vector((end.x - 26, end.y - 2, 30)),
        Vector((end.x - 24, end.y, end.z - 26)),
        Vector((end.x - 14, end.y, end.z - 3)),
        end + Vector((-4, 0, 0)),
        end,
    ]
else:
    # The iPad stands in front of the laptop's port: out of the laptop's side,
    # down behind the dock, along the desk in front of the laptop, and up
    # beside the dock into the iPad's port.
    path = [
        start,
        start + Vector((8, -1, -1.5)),
        Vector((start.x + 16, start.y - 12, r + 0.6)),
        Vector((start.x + 10, start.y - 60, r)),
        Vector((start.x - 30, IPAD_FRONT + 70, r)),
        Vector((end.x + 30, IPAD_FRONT + 40, r)),
        Vector((end.x - 8, IPAD_FRONT + 10, r)),
        Vector((end.x - 22, IPAD_FRONT - 10, r)),
        Vector((end.x - 26, IPAD_FRONT - 2, 10)),
        Vector((end.x - 26, end.y - 2, 32)),
        Vector((end.x - 24, end.y, end.z - 26)),
        Vector((end.x - 14, end.y, end.z - 3)),
        end + Vector((-4, 0, 0)),
        end,
    ]
tube("cable", [tuple(p) for p in path], r, jacket, col_cable)

# Light: a large soft key from the upper left and front, a fill from the
# left, a cool rim from behind on the right, and a broad soft top light that
# lays the contact shadows under everything. None sits where the iPad's
# glass would mirror it back into the lens (to the right of the camera).
light("key", "AREA", (-700, -650, 800), (300, 120, 60), 1.15e7, (800, 600))
light("fill", "AREA", (-1100, -300, 220), (300, 60, 60), 1.4e6, (600, 300))
light("rim", "AREA", (1300, 700, 520), (500, 100, 90), 3.6e6, (300, 600), color=(0.86, 0.92, 1.0))
light("top", "AREA", (250, 150, 1000), (250, 120, 0), 3.0e6, (1100, 600))
light("front", "AREA", (-150, -1300, 60), (300, 100, 20), 0.45e6, (900, 120))
# Behind and above, for the aluminium's top and left edges.
light("crown", "AREA", (350, 900, 700), (400, 120, 120), 2.2e6, (900, 300), color=(0.92, 0.95, 1.0))
light("edge", "AREA", (-200, 700, 260), (300, 140, 90), 1.2e6, (300, 400))

# The camera: the laptop picture's own, placed so its principal point lands
# where that picture's middle sits on this canvas.
cam_data = bpy.data.cameras.new("cam")
cam_data.type = "PERSP"
cam_data.sensor_fit = "HORIZONTAL"
cam_data.sensor_width = 36.0
cam_data.lens = 36.0 * FOCAL / W
ppx, ppy = PP[0] + L["laptop_at"][0], PP[1] + L["laptop_at"][1]
cam_data.shift_x = (W / 2 - ppx) / W
cam_data.shift_y = (ppy - H / 2) / W
cam_data.clip_start = 10
cam_data.clip_end = 8000
cam = bpy.data.objects.new("cam", cam_data)
scene.collection.objects.link(cam)
cam.location = (0, EYE_Y, DECK_TOP + EYE_Z_DECK)
cam.rotation_euler = (math.pi / 2 - PITCH, 0, 0)
scene.camera = cam


bpy.context.view_layer.update()


def canvas(p):
    v = world_to_camera_view(scene, cam, Vector(p))
    return (round(v.x * W, 1), round((1 - v.y) * H, 1))


if CHECK:
    ax, ay = L["laptop_at"]
    print("deck front-top (laptop picture: 159,886 / 1513,886):",
          [tuple(round(c - o, 1) for c, o in zip(canvas((x, 0, DECK_TOP)), (ax, ay))) for x in (-DECK_W / 2, DECK_W / 2)])
    print("deck hinge (285,668 / 1387,668):",
          [tuple(round(c - o, 1) for c, o in zip(canvas((x, DECK_D, DECK_TOP)), (ax, ay))) for x in (-DECK_W / 2, DECK_W / 2)])
    print("desk under the front edge (about 912):", tuple(round(c - o, 1) for c, o in zip(canvas((0, 0, 0)), (ax, ay))))
    lid_top = lid.matrix_world @ Vector((0, -2.25, 91.0))
    print("lid top (row 28):", tuple(round(c - o, 1) for c, o in zip(canvas(lid_top), (ax, ay))))
    corners = [ipad_frame @ Vector((bx + dx, 0, bz + dz)) for dx, dz in ((0, 0), (SCREEN_W, 0), (0, SCREEN_H), (SCREEN_W, SCREEN_H))]
    print("ipad display corners (canvas):", [canvas(c) for c in corners])
    print("ipad body left/right, top:", canvas(ipad_frame @ Vector((0, 0, IPAD_H))), canvas(ipad_frame @ Vector((IPAD_W, 0, IPAD_H))))
    print("dock front-bottom:", canvas((IPAD_LEFT + IPAD_W / 2, IPAD_FRONT + 12 - dock_d / 2, 0)))
    print("cable start/end:", canvas(start), canvas(end))
    sys.exit(0)


def show(collection, camera=True, shadow=True, hold=False, render=True):
    collection.hide_render = not render
    for obj in list(collection.all_objects):
        obj.visible_camera = camera
        obj.visible_shadow = shadow
        obj.is_holdout = hold
    cutter.hide_render = True


def render(name, samples):
    scene.cycles.samples = max(8, samples // 8) if DRAFT else samples
    scene.render.filepath = f"{OUT}/{name}.png"
    bpy.ops.render.render(write_still=True)


show(col_laptop, camera=False)

# Pass 1: the iPad and its dock. No cable, no desk.
show(col_cable, render=False)
show(col_desk, render=False)
show(col_ipad)
render("ipad", 192)

# Pass 2: the desk's shadows alone.
show(col_desk)
show(col_ipad, camera=False)
render("shadow", 128)

# Pass 3: the cable and plugs, held out where the laptop or the iPad and its
# dock stand in front of them. No desk.
show(col_desk, render=False)
show(col_cable)
show(col_ipad, hold=True)
show(col_laptop, hold=True)
render("cable", 192)

# Pass 4: the cable's own shadow on the desk; hero_compose.py lays it over
# the shared one, which makes the two meet the way overlapping shadows do.
show(col_desk)
show(col_cable, camera=False)
show(col_ipad, shadow=False, hold=True)
show(col_laptop, shadow=False, hold=True)
render("cable-shadow", 128)

if "--blend" in sys.argv:
    bpy.ops.wm.save_as_mainfile(filepath=f"{OUT}/hero_scene.blend")
