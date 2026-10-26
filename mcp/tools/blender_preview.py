"""Headless clay preview for one model/rig file.

usage: blender -b --factory-startup --python blender_preview.py -- <src> <out> [res] [elev] [yaw] [action]
Renders a square PNG with a neutral clay material, an auto-fit three-quarter
camera, a key/fill area light pair and a floor plane.
"""

import math
import os
import sys

import bpy
from mathutils import Vector

argv = sys.argv[sys.argv.index("--") + 1:]
src, out = argv[0], argv[1]
res = int(argv[2]) if len(argv) > 2 else 512
elev = float(argv[3]) if len(argv) > 3 else 14.0
yaw = float(argv[4]) if len(argv) > 4 else 34.0
action_filter = argv[5].lower() if len(argv) > 5 else ""

BG = 0.16

bpy.ops.wm.read_factory_settings(use_empty=True)
sc = bpy.context.scene
try:
    sc.render.engine = "BLENDER_EEVEE_NEXT"
except TypeError:
    try:
        sc.render.engine = "BLENDER_EEVEE"
    except TypeError:
        pass
sc.render.resolution_x = res
sc.render.resolution_y = res
sc.render.film_transparent = False
for _vt in ("AgX", "Filmic", "Standard"):
    try:
        sc.view_settings.view_transform = _vt
        break
    except TypeError:
        continue

world = bpy.data.worlds.new("W")
sc.world = world
world.use_nodes = True
bg = world.node_tree.nodes["Background"]
bg.inputs[0].default_value = (BG, BG, BG, 1.0)
bg.inputs[1].default_value = 1.0

ext = os.path.splitext(src)[1].lower()
if ext in (".glb", ".gltf"):
    bpy.ops.import_scene.gltf(filepath=src)
else:
    bpy.ops.import_scene.fbx(filepath=src)

# ---- pick the posing action
arm = next((o for o in bpy.data.objects if o.type == "ARMATURE"), None)
chosen = None
if arm is not None:
    if arm.animation_data is None:
        arm.animation_data_create()
    actions = list(bpy.data.actions)
    if actions:
        pick = [a for a in actions if action_filter and action_filter in a.name.lower()]
        if not pick and not action_filter:
            pick = [a for a in actions if a.name.lower().startswith("idle")]
        if not pick and not action_filter:
            pick = actions
        if pick:
            chosen = pick[0]
            arm.animation_data.action = chosen
            slot = getattr(arm.animation_data, "action_slot", None)
            if slot is None and getattr(chosen, "slots", None):
                try:
                    arm.animation_data.action_slot = chosen.slots[0]
                except Exception:
                    pass
            sc.frame_set(int(chosen.frame_range[0]))
bpy.context.view_layer.update()

# ---- which meshes belong to the asset?
# Rigs exported from some packs carry a stray 2 m "Icosphere" placeholder at the
# scene root. It is not parented to a bone and not skinned, so drop it from the
# framing/material pass (the app drops the same objects).
def _under_rig(o):
    p = o.parent
    while p is not None:
        if p.type == "ARMATURE" or p.parent_type == "BONE":
            return True
        p = p.parent
    return False


meshes = [o for o in bpy.data.objects if o.type == "MESH"]
has_rig = any(o.parent is not None and o.parent.type == "ARMATURE" for o in meshes)
keep = [o for o in meshes if _under_rig(o)] if has_rig else meshes
keep = keep or meshes
for o in meshes:
    if o not in keep:
        o.hide_render = True

# ---- clay
clay = bpy.data.materials.new("clay")
clay.use_nodes = True
bsdf = clay.node_tree.nodes.get("Principled BSDF")
if bsdf:
    bsdf.inputs["Base Color"].default_value = (0.74, 0.74, 0.74, 1.0)
    bsdf.inputs["Roughness"].default_value = 0.55
for o in keep:
    o.data.materials.clear()
    o.data.materials.append(clay)

# ---- bounds (bind/evaluated)
dg = bpy.context.evaluated_depsgraph_get()
pts = []
for o in keep:
    ev = o.evaluated_get(dg)
    for corner in ev.bound_box:
        pts.append(ev.matrix_world @ Vector(corner))
if not pts:
    print("NO_MESH")
    sys.exit(2)
mn = Vector((min(p.x for p in pts), min(p.y for p in pts), min(p.z for p in pts)))
mx = Vector((max(p.x for p in pts), max(p.y for p in pts), max(p.z for p in pts)))
center = (mn + mx) / 2
size = mx - mn
radius = (size.length * 0.5) or 1.0

# ---- camera
cam_data = bpy.data.cameras.new("cam")
cam_data.lens = 60
cam = bpy.data.objects.new("cam", cam_data)
sc.collection.objects.link(cam)
half_fov = math.atan(cam_data.sensor_width * 0.5 / cam_data.lens)
dist = radius / math.sin(half_fov) * 1.25
d = Vector(
    (
        math.cos(math.radians(elev)) * math.sin(math.radians(yaw)),
        -math.cos(math.radians(elev)) * math.cos(math.radians(yaw)),
        math.sin(math.radians(elev)),
    )
)
cam.location = center + d * dist
cam.rotation_euler = (center - cam.location).to_track_quat("-Z", "Y").to_euler()
sc.camera = cam

# ---- lights
def area(name, energy, loc):
    light = bpy.data.lights.new(name, "AREA")
    light.energy = energy
    light.size = radius * 2.5
    obj = bpy.data.objects.new(name, light)
    sc.collection.objects.link(obj)
    obj.location = center + loc
    obj.rotation_euler = (center - obj.location).to_track_quat("-Z", "Y").to_euler()
    return obj


area("key", 260 * radius * radius, Vector((radius * 2.2, -radius * 2.0, radius * 2.2)))
area("fill", 80 * radius * radius, Vector((-radius * 2.2, -radius * 1.4, radius * 0.9)))

# ---- floor
floor_mat = bpy.data.materials.new("floor")
floor_mat.use_nodes = True
fb = floor_mat.node_tree.nodes.get("Principled BSDF")
if fb:
    fb.inputs["Base Color"].default_value = (BG + 0.02, BG + 0.02, BG + 0.02, 1.0)
    fb.inputs["Roughness"].default_value = 1.0
bpy.ops.mesh.primitive_plane_add(size=radius * 30, location=(center.x, center.y, mn.z))
plane = bpy.context.object
plane.data.materials.append(floor_mat)

sc.render.filepath = out
bpy.ops.render.render(write_still=True)
print("ACTION", chosen.name if chosen else "(bind pose)")
print("RENDERED", out)
