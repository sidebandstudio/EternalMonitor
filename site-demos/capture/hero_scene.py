# The home page hero's iPad, its dock and the USB-C cable, rendered in Blender
# to sit beside the laptop render (site-demos/art/hero-laptop.webp).
#
#   blender -b -P site-demos/capture/hero_scene.py -- <out folder> [scale]
#
# Writes ipad.png (the tablet and its dock, the display left transparent) and
# cable.png (the cable and both plugs), each W x H canvas
# pixels times scale. Units are millimetres. The camera is orthographic and
# looks down 15 degrees; one millimetre at the front plane is K canvas pixels,
# the laptop render's own scale, so the two layers line up with it unmoved.
import math
import sys

import bpy
import bmesh
from mathutils import Matrix, Vector

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
OUT = argv[0] if argv else "/tmp"
SCALE = float(argv[1]) if len(argv) > 1 else 2.0

K = 3.72                 # canvas pixels per millimetre
W, H = 2709, 980         # the hero canvas, in canvas pixels
BASE_Y = 914             # canvas row of the desk at the front plane
PITCH = math.radians(15)
LEAN = math.radians(10)  # the iPad leans back this much in its dock

IPAD_W, IPAD_H, IPAD_T, IPAD_R = 249.7, 177.5, 5.3, 12.0
SCREEN_W, SCREEN_H, SCREEN_R = 233.0, 160.5, 7.5
IPAD_LEFT = 1760 / K     # world x of the iPad's left edge
IPAD_LIFT = 12.0         # its bottom edge above the desk, resting in the dock
CABLE_R = 1.9


def canvas_to_world(cx, cy, depth):
    """World point that lands on canvas (cx, cy) at a given depth behind the front plane."""
    v = (BASE_Y - cy) / K
    z = (v - depth * math.sin(PITCH)) / math.cos(PITCH)
    return Vector((cx / K, depth, z))


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
    scene.cycles.samples = 160
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
    a soft-touch boot, then a tapered strain relief. Returns the relief's far end."""
    boot_mat, relief_mat, metal_mat = cable_mats
    parts = []
    shell = box(name + "-shell", (0.8, 8.0, 2.4), metal_mat, collection, bevel=0.4)
    shell.location = (0.2, 0, 0)
    parts.append(shell)
    boot = box(name + "-boot", (17.0, 10.6, 5.0), boot_mat, collection, bevel=2.1)
    boot.location = (9.1, 0, 0)
    parts.append(boot)
    seam = box(name + "-seam", (0.5, 10.9, 5.3), relief_mat, collection, bevel=0.2)
    seam.location = (4.0, 0, 0)
    parts.append(seam)
    relief = tube(name + "-relief", [(17.0, 0, 0), (20.0, 0, 0), (23.0, 0, 0), (26.0, 0, 0)], 3.0, relief_mat, collection, r_end=CABLE_R + 0.15)
    parts.append(relief)
    for p in parts:
        p.matrix_world = frame @ p.matrix_basis
    return frame @ Vector((26.0, 0, 0))


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
col_env = bpy.data.collections.new("env")
for c in (col_ipad, col_cable, col_env):
    scene.collection.children.link(c)

alu = material("alu", (0.55, 0.55, 0.57), metallic=1.0, roughness=0.36, aniso=0.2)
alu_dock = material("alu-dock", (0.6, 0.6, 0.62), metallic=1.0, roughness=0.55)
glass = material("glass", (0.004, 0.004, 0.005), roughness=0.04, coat=1.0)
camera_dot = material("camera-dot", (0.015, 0.018, 0.03), roughness=0.1, coat=1.0)
screen = holdout("screen")
rubber = material("rubber", (0.80, 0.80, 0.78), roughness=0.55, coat=0.08)
rubber_boot = material("boot", (0.86, 0.86, 0.85), roughness=0.42, coat=0.15)
seam = material("seam", (0.55, 0.55, 0.55), roughness=0.5)
steel = material("steel", (0.75, 0.75, 0.76), metallic=1.0, roughness=0.2)

# The iPad, built in its own frame: x across, z up, front face at y = 0.
ipad_frame = Matrix.Translation((IPAD_LEFT, 0, IPAD_LIFT)) @ Matrix.Rotation(-LEAN, 4, "X")
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

# The dock: a low aluminium bar the iPad's bottom edge rests in.
dock_w, dock_d, dock_h = 150.0, 44.0, 16.0
dock = box("dock", (dock_w, dock_d, dock_h), alu_dock, col_ipad, bevel=2.4)
dock.location = (IPAD_LEFT + IPAD_W / 2, 8.0, dock_h / 2)

# Plugs. The laptop's port is in its right side, under the deck's edge at
# canvas (1332, 819); the iPad's is in the middle of its left edge.
laptop_port = canvas_to_world(1332, 819, 76.6)
laptop_port.x -= 4.0  # the plug's inner end hides behind the laptop's side
laptop_frame = Matrix.Translation(laptop_port)
start = plug("laptop-plug", laptop_frame, (rubber_boot, seam, steel), col_cable)
ipad_port = ipad_frame @ Vector((0, IPAD_T / 2, IPAD_H / 2))
# Out of the iPad's left edge: local -x, with the plug's flat side facing the camera.
# The plug's axes: x out of the port, y across its wide side, z across its thin side.
out_of_left_edge = Matrix(((-1, 0, 0), (0, 0, 1), (0, 1, 0))).to_4x4()
ipad_plug_frame = Matrix.Translation(ipad_port) @ ipad_frame.to_3x3().to_4x4() @ out_of_left_edge
end = plug("ipad-plug", ipad_plug_frame, (rubber_boot, seam, steel), col_cable)

# Out of the laptop, down to the desk, a shallow arc toward the camera, then
# up beside the iPad and round into its port.
r = CABLE_R
path = [
    start,
    start + Vector((9, -1, -1)),
    Vector((395, 68, r + 0.8)),
    Vector((399, 44, r)),
    Vector((408, 14, r)),
    Vector((418, -6, r)),
    Vector((428, -2, r)),
    Vector((434, 8, r + 0.5)),
    Vector((436, 12, 10)),
    Vector((434, 15, 35)),
    Vector((433, 17, 65)),
    Vector((436, 17.8, 88)),
    Vector((end.x - 5, end.y, end.z - 2)),
    end + Vector((-3, 0, 0)),
    end,
]
tube("cable", [tuple(p) for p in path], r, rubber, col_cable)

# Light: a large soft key from the upper left and front, a fill from the
# left, and a cool rim from behind on the right, as in the laptop render.
light("key", "AREA", (-250, -650, 650), (450, 60, 60), 9.0e6, (700, 500))
# The fill stays well off the line the iPad's glass mirrors back to the camera.
light("fill", "AREA", (-700, -350, 150), (450, 0, 60), 1.6e6, (600, 300))
light("rim", "AREA", (1150, 450, 420), (550, 0, 90), 3.2e6, (300, 500), color=(0.86, 0.92, 1.0))
light("top", "AREA", (450, 100, 900), (450, 50, 0), 2.0e6, (900, 400))
# A low softbox in front lights the dock's face and the aluminium edges. It sits
# below the line the glass mirrors back to the camera (5 degrees up), so the
# bezel stays black.
light("low", "AREA", (560, -700, -160), (560, 0, 20), 0.35e6, (900, 160))

cam_data = bpy.data.cameras.new("cam")
cam_data.type = "ORTHO"
cam_data.ortho_scale = W / K
cam_data.sensor_fit = "HORIZONTAL"
cam_data.clip_start = 1
cam_data.clip_end = 6000
cam = bpy.data.objects.new("cam", cam_data)
scene.collection.objects.link(cam)
up = Vector((0, math.sin(PITCH), math.cos(PITCH)))
fwd = Vector((0, math.cos(PITCH), -math.sin(PITCH)))
centre = Vector(((W / 2) / K, 0, 0)) + up * ((BASE_Y - H / 2) / K)
cam.location = centre - fwd * 2500
cam.rotation_euler = (math.pi / 2 - PITCH, 0, 0)
scene.camera = cam


def set_visible(collection, visible, shadow=True, hold=False):
    collection.hide_render = not visible
    for obj in collection.all_objects:
        obj.visible_shadow = shadow
        obj.is_holdout = hold


# Pass 1: the iPad and its dock. No cable.
set_visible(col_cable, False)
set_visible(col_ipad, True)
scene.render.filepath = f"{OUT}/ipad.png"
bpy.ops.render.render(write_still=True)

# Pass 2: the cable and plugs. hero_compose.py cuts it with the iPad's layer.
set_visible(col_cable, True)
set_visible(col_ipad, True, shadow=False, hold=True)
scene.render.filepath = f"{OUT}/cable.png"
bpy.ops.render.render(write_still=True)

if "--blend" in sys.argv:
    bpy.ops.wm.save_as_mainfile(filepath=f"{OUT}/hero_scene.blend")
