"""Transfer assets into the running Unity editor through its MCP endpoint.

The import plan is a small TSV (one `P` line per placement, `M` lines for its
materials) so the C# side parses it with plain string splitting - JsonUtility
is unreliable for nested lists.
"""

from __future__ import annotations

import json
import os
import shutil
import time
import urllib.error
import urllib.request
from pathlib import Path

from .library import LIB, ROOT, TARGETS, _load_json

UNITY_URL = os.environ.get("ASSET_LIBRARY_UNITY_URL", "http://localhost:26440")
IMPORT_ROOT = "Assets/AssetLib/Imported"
# Mirrored import area used by the Unity-side asset library (Assets/AssetLib/<library path>).
ASSET_ROOT = "Assets/AssetLib"


class UnityError(RuntimeError):
    pass


def execute(csharp: str, class_name: str = "AssetLibraryTool", method_name: str = "Run",
            timeout: float = 180) -> str:
    payload = json.dumps(
        {"csharpCode": csharp, "className": class_name, "methodName": method_name, "isMethodBody": False}
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{UNITY_URL}/api/tools/script-execute",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            doc = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", "replace")
        raise UnityError(f"Unity rejected the script: {body[:600]}") from exc
    except urllib.error.URLError as exc:
        raise UnityError(f"Unity editor is not reachable at {UNITY_URL}: {exc}") from exc

    value = ((doc.get("structured") or {}).get("result") or {}).get("value")
    if value is None:
        blocks = doc.get("content") or []
        text = blocks[0].get("text") if blocks and isinstance(blocks[0], dict) else None
        if text:
            if text.startswith("System.String:"):
                text = text.split(":", 1)[1].strip()
            if text.startswith('"') and text.endswith('"'):
                text = text[1:-1].encode("utf-8").decode("unicode_escape")
            value = text
    if value is None:
        raise UnityError(f"unexpected Unity MCP response: {str(doc)[:400]}")
    return str(value)


def status() -> dict:
    code = """
using System;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;

public class AssetLibraryStatus
{
    public static string Run()
    {
        var sc = EditorSceneManager.GetActiveScene();
        return Application.dataPath + "\\n" + Application.unityVersion + "\\n" + sc.path + "\\n" + sc.name;
    }
}
"""
    out = execute(code, "AssetLibraryStatus").split("\n")
    project = Path(out[0]).parent if out and out[0] else None
    return {
        "online": True,
        "project": str(project) if project else None,
        "unity_version": out[1] if len(out) > 1 else "",
        "scene_path": out[2] if len(out) > 2 else "",
        "scene_name": out[3] if len(out) > 3 else "",
    }


def _unity_target() -> dict:
    return _load_json(TARGETS / "unity.json") or {}


def _copy_asset(it: dict, dest_root: Path) -> dict:
    dest_root.mkdir(parents=True, exist_ok=True)
    fbx_src = LIB.fbx_paths(it)
    if not fbx_src:
        raise UnityError(f"{it.get('code')}: no FBX on disk")
    fbx_dst = dest_root / fbx_src[0].name
    shutil.copy2(fbx_src[0], fbx_dst)
    tex_dir = dest_root / "Textures"
    tex_dir.mkdir(exist_ok=True)
    copied: dict[str, str] = {}
    for t in LIB.texture_paths(it):
        src = Path(t["path"])
        if not src.exists():
            continue
        dst = tex_dir / src.name
        shutil.copy2(src, dst)
        copied[str(src)] = str(dst)

    # The library preview travels with the asset so Unity can show the same thumbnail.
    preview = None
    src_preview = LIB.preview_path(it)
    if src_preview and src_preview.exists():
        dst = dest_root / src_preview.name
        shutil.copy2(src_preview, dst)
        preview = str(dst)

    # The sidecar keeps the material spec next to the imported model.
    side = LIB.asset_dir(it) / f"{it.get('slug')}.json"
    if side.exists():
        shutil.copy2(side, dest_root / side.name)

    return {"fbx": str(fbx_dst), "textures": copied, "preview": preview}


_CSHARP = """
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;

public class AssetLibraryImport
{
    const string PLAN = "@PLAN@";
    const string SHADER = "@SHADER@";

    class MatSpec
    {
        public string name; public string metallic; public string smoothness; public string transparent;
        public string baseColor; public string normal;
    }
    class Entry
    {
        public string code; public string slug; public string assetPath; public Vector3 pos;
        public float rotY; public List<MatSpec> mats = new List<MatSpec>();
    }

    public static string Run()
    {
        var sb = new StringBuilder();
        var entries = Parse(File.ReadAllLines(PLAN));
        AssetDatabase.Refresh(ImportAssetOptions.ForceUpdate);
        int created = 0;
        foreach (var e in entries)
        {
            var model = AssetDatabase.LoadAssetAtPath<GameObject>(e.assetPath);
            if (model == null) { sb.AppendLine("MISS " + e.code + " " + e.assetPath); continue; }
            string dir = Path.GetDirectoryName(e.assetPath).Replace('\\\\', '/');

            // Library preview that came in with the FBX: thumbnail settings, no mipmaps.
            if (ConfigurePreview(dir + "/" + e.slug + ".png")) sb.AppendLine("PREVIEW " + e.code);

            var mats = new List<Material>();
            int mi = 0;
            foreach (var spec in e.mats)
            {
                string matPath = dir + "/Mat_" + (mi++) + "_" + Sanitize(spec.name) + ".mat";
                var mat = AssetDatabase.LoadAssetAtPath<Material>(matPath);
                if (mat == null)
                {
                    var shader = Shader.Find(SHADER);
                    if (shader == null) { sb.AppendLine("NOSHADER " + SHADER); break; }
                    mat = new Material(shader);
                    AssetDatabase.CreateAsset(mat, matPath);
                }
                if (!string.IsNullOrEmpty(spec.baseColor))
                {
                    var tex = AssetDatabase.LoadAssetAtPath<Texture2D>(spec.baseColor);
                    if (tex != null) mat.SetTexture("_BaseMap", tex);
                }
                if (!string.IsNullOrEmpty(spec.normal))
                {
                    var tex = AssetDatabase.LoadAssetAtPath<Texture2D>(spec.normal);
                    if (tex != null)
                    {
                        mat.SetTexture("_BumpMap", tex);
                        mat.EnableKeyword("_NORMALMAP");
                    }
                }
                mat.SetFloat("_Metallic", ParseF(spec.metallic, 0f));
                mat.SetFloat("_Smoothness", ParseF(spec.smoothness, 0.5f));
                if (spec.transparent == "1")
                {
                    mat.SetFloat("_Surface", 1f);
                    mat.SetFloat("_SrcBlend", (float)UnityEngine.Rendering.BlendMode.SrcAlpha);
                    mat.SetFloat("_DstBlend", (float)UnityEngine.Rendering.BlendMode.OneMinusSrcAlpha);
                    mat.SetFloat("_ZWrite", 0f);
                    mat.EnableKeyword("_SURFACE_TYPE_TRANSPARENT");
                    mat.renderQueue = (int)UnityEngine.Rendering.RenderQueue.Transparent;
                }
                EditorUtility.SetDirty(mat);
                mats.Add(mat);
            }

            var inst = (GameObject)PrefabUtility.InstantiatePrefab(model);
            inst.name = e.code + " " + e.slug;
            inst.transform.position = e.pos;
            inst.transform.rotation = Quaternion.Euler(0f, e.rotY, 0f);
            foreach (var r in inst.GetComponentsInChildren<Renderer>())
            {
                var slots = r.sharedMaterials;
                for (int i = 0; i < slots.Length && i < mats.Count; i++) slots[i] = mats[i];
                r.sharedMaterials = slots;
            }
            Undo.RegisterCreatedObjectUndo(inst, "Import " + e.code);
            created++;
            sb.AppendLine("OK " + e.code + " " + inst.name + " mats=" + mats.Count);
        }
        AssetDatabase.SaveAssets();
        EditorSceneManager.MarkSceneDirty(EditorSceneManager.GetActiveScene());
        sb.AppendLine("CREATED " + created);
        return sb.ToString();
    }

    static List<Entry> Parse(string[] lines)
    {
        var list = new List<Entry>();
        Entry cur = null;
        foreach (var raw in lines)
        {
            if (string.IsNullOrEmpty(raw)) continue;
            var f = raw.Split('\\t');
            if (f[0] == "P")
            {
                cur = new Entry
                {
                    assetPath = f[1],
                    pos = new Vector3(ParseF(f[2], 0f), ParseF(f[3], 0f), ParseF(f[4], 0f)),
                    rotY = ParseF(f[5], 0f),
                    code = f[6],
                    slug = f[7],
                };
                list.Add(cur);
            }
            else if (f[0] == "M" && cur != null)
            {
                cur.mats.Add(new MatSpec
                {
                    name = f[1],
                    metallic = f[2],
                    smoothness = f[3],
                    transparent = f[4],
                    baseColor = f[5],
                    normal = f[6],
                });
            }
        }
        return list;
    }

    static float ParseF(string s, float fallback)
    {
        float v;
        return float.TryParse(s, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out v) ? v : fallback;
    }

    /// <summary>Preview PNG → thumbnail texture (no mipmaps, 512 clamp, sRGB). Returns false when absent.</summary>
    static bool ConfigurePreview(string path)
    {
        var importer = AssetImporter.GetAtPath(path) as TextureImporter;
        if (importer == null) return false;
        bool changed = false;
        if (importer.mipmapEnabled) { importer.mipmapEnabled = false; changed = true; }
        if (importer.maxTextureSize > 512) { importer.maxTextureSize = 512; changed = true; }
        if (!importer.sRGBTexture) { importer.sRGBTexture = true; changed = true; }
        if (importer.wrapMode != TextureWrapMode.Clamp) { importer.wrapMode = TextureWrapMode.Clamp; changed = true; }
        if (changed) importer.SaveAndReimport();
        return true;
    }

    static string Sanitize(string s)
    {
        if (string.IsNullOrEmpty(s)) return "mat";
        var sb = new StringBuilder();
        foreach (var c in s) sb.Append(char.IsLetterOrDigit(c) ? c : '_');
        return sb.ToString();
    }
}
"""


def import_assets(codes: list[str], spacing: float = 2.0, columns: int = 5, rot_y: float = 0.0,
                  layout: str = "grid") -> dict:
    """Copy each asset's FBX + textures in, build materials, place them.

    `layout`: `grid` (columns per row), `row` (single row) or `ring` (a circle
    of radius `spacing * count / pi`, handy for turntable review).
    """
    st = status()
    project = Path(st["project"]) if st.get("project") else None
    if not project:
        raise UnityError("could not resolve the Unity project path")
    shader = ((_unity_target().get("material") or {}).get("shader")) or "Universal Render Pipeline/Lit"

    lines: list[str] = []
    skipped: list[dict] = []
    previews = 0
    for n, code in enumerate(codes):
        it = LIB.get(code)
        if not it:
            skipped.append({"code": code, "error": "not found"})
            continue
        if it.get("kind") == "material":
            skipped.append({"code": code, "error": "materials are not importable as models"})
            continue
        folder = f"{IMPORT_ROOT}/{it['code']}_{it['slug']}"
        dest = project / folder.replace("/", os.sep)
        copy = _copy_asset(it, dest)
        if copy.get("preview"):
            previews += 1
        fbx_rel = f"{folder}/{Path(copy['fbx']).name}"
        if layout == "ring":
            import math

            angle = 2 * math.pi * n / max(1, len(codes))
            radius = max(spacing, spacing * len(codes) / math.pi)
            x, z = radius * math.cos(angle), radius * math.sin(angle)
            face = -math.degrees(angle) + 90.0
        elif layout == "row":
            x, z, face = n * spacing, 0.0, rot_y
        else:
            row, col = divmod(n, max(1, columns))
            x, z, face = col * spacing, -row * spacing, rot_y
        lines.append(f"P\t{fbx_rel}\t{x:.4f}\t0\t{z:.4f}\t{face:.4f}\t{it['code']}\t{it['slug']}")
        for m in (LIB.sidecar(it).get("materials") or []):
            base = normal = ""
            for kind, rel in (m.get("textures") or {}).items():
                if not rel:
                    continue
                dst = copy["textures"].get(str(ROOT / str(rel).replace("/", os.sep)))
                if not dst:
                    continue
                rel_asset = Path(dst)
                try:
                    rel_asset = rel_asset.relative_to(project)
                except ValueError:
                    pass
                rel_str = str(rel_asset).replace(os.sep, "/")
                if kind in ("base_color", "basecolor", "albedo"):
                    base = rel_str
                elif kind == "normal":
                    normal = rel_str
            params = m.get("params") or {}
            transp = "1" if (m.get("transparency") or {}).get("enabled") else "0"
            lines.append(
                "M\t{name}\t{metallic}\t{smoothness}\t{transp}\t{base}\t{normal}".format(
                    name=m.get("name") or "Material",
                    metallic=params.get("metallic", 0.0),
                    smoothness=params.get("smoothness", 0.5),
                    transp=transp,
                    base=base,
                    normal=normal,
                )
            )

    plan = Path(os.environ.get("TEMP", ".")) / f"asset-import-{int(time.time() * 1000)}.tsv"
    plan.write_text("\n".join(lines), encoding="utf-8")
    code = _CSHARP.replace("@PLAN@", str(plan).replace("\\", "/")).replace("@SHADER@", shader)
    out = execute(code, "AssetLibraryImport")
    return {
        "requested": len(codes),
        "imported": sum(1 for line in out.splitlines() if line.startswith("OK ")),
        "previews": previews,
        "unity": st,
        "plan": str(plan),
        "log": out.strip().splitlines()[-8:],
        "skipped": skipped,
    }


_RIG_CSHARP = """
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using UnityEditor;
using UnityEditor.Animations;
using UnityEditor.SceneManagement;
using UnityEngine;

public class AssetLibraryRigImport
{
    const string FBX = "@FBX@";
    const string NAME = "@NAME@";
    const string CLIPS = "@CLIPS@";
    const string CLIPNAMES = "@CLIPNAMES@";
    const float PX = @X@;
    const float PY = @Y@;
    const float PZ = @Z@;

    public static string Run()
    {
        AssetDatabase.Refresh(ImportAssetOptions.ForceUpdate);
        var importer = AssetImporter.GetAtPath(FBX) as ModelImporter;
        if (importer == null) return "NO_IMPORTER " + FBX;
        importer.animationType = ModelImporterAnimationType.Human;
        importer.importAnimation = true;
        importer.SaveAndReimport();

        var assets = AssetDatabase.LoadAllAssetsAtPath(FBX);
        var avatar = assets.OfType<Avatar>().FirstOrDefault();
        var all = assets.OfType<AnimationClip>()
            .Where(c => !c.name.StartsWith("__preview__") && !c.name.StartsWith("BindPose"))
            .ToList();
        var wanted = string.IsNullOrEmpty(CLIPS)
            ? all
            : all.Where(c => CLIPS.Split(',').Contains(c.name)).ToList();

        // The rig's own FBX often carries no takes (the pack keeps its clips in
        // sibling animation files). Fall back to finding clips by name anywhere
        // in the project, using the names the library already knows.
        if (wanted.Count == 0 && !string.IsNullOrEmpty(CLIPNAMES))
        {
            var seen = new HashSet<string>();
            foreach (var n in CLIPNAMES.Split(','))
            {
                foreach (var g in AssetDatabase.FindAssets(n + " t:Model"))
                {
                    var p = AssetDatabase.GUIDToAssetPath(g);
                    if (!seen.Add(p)) continue;
                    foreach (var c in AssetDatabase.LoadAllAssetsAtPath(p).OfType<AnimationClip>())
                    {
                        if (c.name == n) wanted.Add(c);
                    }
                }
            }
        }

        string dir = Path.GetDirectoryName(FBX).Replace('\\\\', '/');
        string ctrlPath = dir + "/" + NAME + "_Controller.controller";
        AssetDatabase.DeleteAsset(ctrlPath);
        var ctrl = AnimatorController.CreateAnimatorControllerAtPath(ctrlPath);
        foreach (var c in wanted) ctrl.AddMotion(c);

        var model = AssetDatabase.LoadAssetAtPath<GameObject>(FBX);
        var inst = (GameObject)PrefabUtility.InstantiatePrefab(model);
        inst.name = NAME;
        inst.transform.position = new Vector3(PX, PY, PZ);
        var anim = inst.GetComponent<Animator>();
        if (anim == null) anim = inst.AddComponent<Animator>();
        if (anim.avatar == null && avatar != null) anim.avatar = avatar;
        anim.runtimeAnimatorController = ctrl;

        Undo.RegisterCreatedObjectUndo(inst, "Import rig " + NAME);
        EditorSceneManager.MarkSceneDirty(EditorSceneManager.GetActiveScene());
        AssetDatabase.SaveAssets();
        return "OK " + NAME + " avatar=" + (avatar != null ? avatar.isHuman.ToString() : "none")
             + " clips=" + wanted.Count + " controller=" + ctrlPath;
    }
}
"""


def import_rig(code: str, clips: list[str] | None = None, x: float = 0.0, y: float = 0.0, z: float = 0.0) -> dict:
    """Import a rig into Unity as a Humanoid avatar + Animator + clip controller."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    if it.get("kind") != "rig":
        return {"error": f"{code} is a {it.get('kind')}, not a rig"}
    st = status()
    project = Path(st["project"]) if st.get("project") else None
    if not project:
        raise UnityError("could not resolve the Unity project path")
    folder = f"{IMPORT_ROOT}/{it['code']}_{it['slug']}"
    copy = _copy_asset(it, project / folder.replace("/", os.sep))
    fbx_rel = f"{folder}/{Path(copy['fbx']).name}"
    name = f"{it['code']} {it['slug']}"
    library_clips = [c["name"] for c in LIB.clips(it)]
    names_for_lookup = [n for n in (clips or library_clips)][:40]
    code_cs = (
        _RIG_CSHARP.replace("@FBX@", fbx_rel)
        .replace("@NAME@", name)
        .replace("@CLIPS@", ",".join(clips or []))
        .replace("@CLIPNAMES@", ",".join(names_for_lookup))
        .replace("@X@", f"{x:.4f}f")
        .replace("@Y@", f"{y:.4f}f")
        .replace("@Z@", f"{z:.4f}f")
    )
    out = execute(code_cs, "AssetLibraryRigImport")
    return {"code": it["code"], "unity": st, "log": out.strip().splitlines()[-4:],
            "clip_count": len(LIB.clips(it))}


def import_scene(pack: str = "", category: str = "", kind: str = "model", limit: int = 24,
                 layout: str = "grid", spacing: float = 2.0, columns: int = 5) -> dict:
    """Resolve a pack/category and import the whole set with a layout."""
    rows = LIB.resolve(pack=pack, category=category, kind=kind, limit=limit)
    codes = [r["code"] for r in rows]
    if not codes:
        return {"error": "no assets matched", "pack": pack, "category": category}
    result = import_assets(codes, spacing=spacing, columns=columns, layout=layout)
    result["resolved"] = codes
    return result


_SYNC_CSHARP = """
using System;
using UnityEditor;
using UnityEngine;

public class AssetLibraryPreviewSync
{
    public static string Run()
    {
        return HKLGame.AssetLib.AssetLibraryBridge.@@METHOD@@();
    }
}
"""


def sync_previews(force: bool = False) -> dict:
    """Ask the Unity editor-side asset library to re-pull every imported preview."""
    method = "SyncPreviewsForce" if force else "SyncPreviews"
    out = execute(_SYNC_CSHARP.replace("@@METHOD@@", method), "AssetLibraryPreviewSync")
    return {"unity": status(), "result": out}


def _csharp_str(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _mirror_source(project: Path, src: Path, force: bool) -> str:
    """Copy one library file into `Assets/AssetLib/<same relative path>`. Returns copied/skipped."""
    try:
        rel = src.resolve().relative_to(ROOT)
    except ValueError:
        return "outside"
    dst = project / ASSET_ROOT / rel
    if (
        not force
        and dst.exists()
        and dst.stat().st_size == src.stat().st_size
        and src.stat().st_mtime <= dst.stat().st_mtime + 1
    ):
        return "skipped"
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    return "copied"


def stage_batch(codes: list[str], force: bool = False) -> dict:
    """Lay out a whole set for import: every FBX/LOD, texture, sidecar and preview of every
    asset is copied into `Assets/AssetLib/<library path>` in one pass, before Unity is asked
    to do anything. Unity then imports the prepared tree instead of analysing asset by asset."""
    st = status()
    project = Path(st["project"]) if st.get("project") else None
    if not project:
        raise UnityError("could not resolve the Unity project path")

    files = copied = skipped = 0
    problems: list[dict] = []
    assets = 0
    for code in codes:
        it = LIB.get(code)
        if not it:
            problems.append({"code": code, "error": "not found"})
            continue
        assets += 1
        base = LIB.asset_dir(it)
        slug = it.get("slug") or ""
        sources: list[Path] = []
        if base.is_dir():
            sources.extend(sorted(base.glob(f"{slug}*.fbx")))
            for name in (f"{slug}.json", f"{slug}.png", "preview.png"):
                candidate = base / name
                if candidate.exists():
                    sources.append(candidate)
        for rel in it.get("textures") or []:
            texture = ROOT / str(rel).replace("/", os.sep)
            if texture.exists():
                sources.append(texture)

        for src in sources:
            files += 1
            state = _mirror_source(project, src, force)
            if state == "copied":
                copied += 1
            elif state == "skipped":
                skipped += 1
            else:
                problems.append({"file": str(src), "error": "outside the library root"})

    return {
        "project": str(project),
        "assets": assets,
        "files": files,
        "copied": copied,
        "skipped": skipped,
        "problems": problems[:10],
    }


_BULK_CSHARP = """
using System;
using UnityEditor;
using UnityEngine;

public class AssetLibraryBulkCall
{
    public static string Run()
    {
        return HKLGame.AssetLib.AssetLibraryBridge.@@METHOD@@(@@CSV@@);
    }
}
"""


def bulk_import(codes: list[str], force: bool = False) -> dict:
    """Stage the whole set on disk, then import it in ONE Unity transaction.

    This is the batch entry point: nothing is imported asset by asset on the Unity side, and
    Unity never copies files - it receives a prepared tree plus one code list.
    """
    wanted = [c for c in dict.fromkeys(codes) if LIB.get(c)]
    if not wanted:
        return {"error": "no importable codes", "requested": codes}

    staged = stage_batch(wanted, force=force)
    method = "BulkImportForce" if force else "BulkImport"
    raw = execute(
        _BULK_CSHARP.replace("@@METHOD@@", method).replace("@@CSV@@", _csharp_str(",".join(wanted))),
        "AssetLibraryBulkCall",
        timeout=900,
    )
    try:
        report = json.loads(raw)
    except json.JSONDecodeError:
        return {"error": "unexpected Unity reply", "raw": raw[:400], "staged": staged}

    report["staged"] = staged
    if len(wanted) != len(codes):
        report["skipped"] = [c for c in codes if c not in wanted]
    return report


def import_native(codes: list[str], timeout: float = 1800.0, poll: float = 0.0) -> dict:
    """Import through the editor-side asset library as one staged batch (see `bulk_import`)."""
    if not codes:
        return {"error": "no codes"}
    return bulk_import(codes)
