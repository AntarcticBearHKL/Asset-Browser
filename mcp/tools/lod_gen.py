"""Headless LOD generation: import an FBX, decimate, export an FBX LOD.

usage: blender -b --factory-startup --python lod_gen.py -- <src.fbx> <out.fbx> <ratio>
"""

import sys

import bpy

argv = sys.argv[sys.argv.index("--") + 1:]
src, out, ratio = argv[0], argv[1], float(argv[2])

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.fbx(filepath=src)

def _under_rig(o):
    p = o.parent
    while p is not None:
        if p.type == "ARMATURE" or p.parent_type == "BONE":
            return True
        p = p.parent
    return False


before = 0
meshes = [o for o in bpy.data.objects if o.type == "MESH"]
# ignore stray root-level placeholders (a 2 m "Icosphere" ships in some packs)
if any(o.parent is not None and o.parent.type == "ARMATURE" for o in meshes):
    keep = [o for o in meshes if _under_rig(o)]
    meshes = keep or meshes
for obj in meshes:
    before += len(obj.data.polygons)
    bpy.context.view_layer.objects.active = obj
    mod = obj.modifiers.new("lod", "DECIMATE")
    mod.decimate_type = "COLLAPSE"
    mod.ratio = ratio
    bpy.ops.object.modifier_apply(modifier=mod.name)

after = sum(len(o.data.polygons) for o in meshes)
bpy.ops.export_scene.fbx(
    filepath=out,
    use_selection=False,
    apply_scale_options="FBX_SCALE_NONE",
    bake_anim=False,
    path_mode="COPY",
    embed_textures=False,
)
print(f"LOD {out} faces {before} -> {after}")
