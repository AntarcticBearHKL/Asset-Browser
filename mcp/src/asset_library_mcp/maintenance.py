"""Write-side maintenance: metadata edits, preview re-renders, LODs, textures."""

from __future__ import annotations

import io
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

from .library import LIB, MANIFEST, ROOT, _load_json

BLENDER = os.environ.get("ASSET_LIBRARY_BLENDER", r"D:\SteamLibrary\steamapps\common\Blender\blender.exe")
TOOLS = Path(__file__).resolve().parent.parent.parent / "tools"
BACKUPS = Path(os.environ.get("TEMP", ".")) / "opencode" / "asset-library-backups"


def _save_json(path: Path, doc) -> None:
    path.write_text(json.dumps(doc, ensure_ascii=False, indent=1), encoding="utf-8")


def _backup(path: Path) -> Path:
    BACKUPS.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dst = BACKUPS / f"{path.stem}-{stamp}{path.suffix}"
    shutil.copy2(path, dst)
    return dst


def _blender(script: Path, args: list, timeout: int = 600) -> subprocess.CompletedProcess:
    cmd = [BLENDER, "--background", "--factory-startup", "--python", str(script), "--"] + [str(a) for a in args]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


# ----------------------------------------------------------------- metadata
def update_asset(code: str, tags: list[str] | None = None, l1: str | None = None,
                 l2: str | None = None, description: str | None = None) -> dict:
    """Edit an asset's tags / category / description in the sidecar AND the manifest."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    side_path = LIB.asset_dir(it) / f"{it.get('slug')}.json"
    side = _load_json(side_path) or {}

    new_tags = sorted({t.strip() for t in (tags if tags is not None else (it.get("tags") or [])) if t.strip()})
    new_l1 = (l1 or it.get("l1") or "").strip()
    new_l2 = (l2 or it.get("l2") or "").strip()
    new_cat = f"{new_l1}/{new_l2}" if new_l2 else new_l1
    new_desc = description if description is not None else side.get("description")

    man = _load_json(MANIFEST) or {}
    entry = next((x for x in man.get("items") or [] if x.get("code") == it.get("code")), None)
    if entry is None:
        return {"error": f"{code} not in the manifest"}
    entry["tags"] = new_tags
    entry["l1"] = new_l1
    entry["l2"] = new_l2
    entry["category"] = new_cat

    if side_path.exists():
        side["tags"] = new_tags
        side["category"] = new_cat
        if new_desc is not None:
            side["description"] = new_desc
        _save_json(side_path, side)
    _save_json(MANIFEST, man)
    LIB._mtime = 0.0  # force a reload on the next read

    return {
        "code": it.get("code"),
        "slug": it.get("slug"),
        "tags": new_tags,
        "category": new_cat,
        "description": new_desc,
        "sidecar": str(side_path),
        "manifest": str(MANIFEST),
    }


# ----------------------------------------------------------------- previews
def regenerate_preview(codes: list[str], res: int = 512) -> dict:
    """Re-render the library preview PNG for each asset with headless Blender."""
    script = TOOLS / "blender_preview.py"
    if not script.exists():
        return {"error": f"missing {script}"}
    results = []
    for code in codes[:12]:
        it = LIB.get(code)
        if not it:
            results.append({"code": code, "error": "not found"})
            continue
        src = None
        if it.get("kind") == "rig" and it.get("glb"):
            src = ROOT / str(it["glb"]).replace("/", "\\")
        if src is None or not src.exists():
            fbx = LIB.fbx_paths(it)
            src = fbx[0] if fbx else None
        if src is None or not src.exists():
            results.append({"code": code, "error": "no source file"})
            continue
        tmp = Path(os.environ.get("TEMP", ".")) / f"preview-{it.get('slug')}-{res}.png"
        try:
            proc = _blender(script, [src, tmp, res], timeout=420)
        except subprocess.TimeoutExpired:
            results.append({"code": code, "error": "blender timeout"})
            continue
        if not tmp.exists():
            results.append({"code": code, "error": "render failed", "stderr": (proc.stderr or "")[-400:]})
            continue
        dest = LIB.preview_path(it) or (ROOT / str(it.get("preview")).replace("/", os.sep))
        dest.parent.mkdir(parents=True, exist_ok=True)
        if dest.exists():
            _backup(dest)
        shutil.copy2(tmp, dest)
        tmp.unlink(missing_ok=True)
        results.append({"code": code, "preview": str(dest), "bytes": dest.stat().st_size})
    return {"rendered": sum(1 for r in results if "preview" in r), "results": results}


# ---------------------------------------------------------------------- LODs
def generate_lods(codes: list[str], ratios: tuple[float, ...] = (0.5, 0.25)) -> dict:
    """Decimate each model's base FBX into `_LOD1/_LOD2` FBX files and record them."""
    script = TOOLS / "lod_gen.py"
    if not script.exists():
        return {"error": f"missing {script}"}
    results = []
    for code in codes[:12]:
        it = LIB.get(code)
        if not it or it.get("kind") != "model":
            results.append({"code": code, "error": "not a model"})
            continue
        base = LIB.fbx_paths(it)
        if not base:
            results.append({"code": code, "error": "no FBX"})
            continue
        src = base[0]
        out_names = [src.name]
        made = []
        for i, ratio in enumerate(ratios, start=1):
            dst = src.with_name(f"{src.stem}_LOD{i}{src.suffix}")
            try:
                proc = _blender(script, [src, dst, ratio], timeout=600)
            except subprocess.TimeoutExpired:
                results.append({"code": code, "error": "blender timeout"})
                break
            if not dst.exists():
                results.append({"code": code, "error": "decimate failed", "stderr": (proc.stderr or "")[-300:]})
                break
            made.append(dst.name)
            out_names.append(dst.name)
            fbm = dst.with_suffix(".fbm")
            if fbm.is_dir():
                shutil.rmtree(fbm, ignore_errors=True)
        if made:
            side_path = LIB.asset_dir(it) / f"{it.get('slug')}.json"
            side = _load_json(side_path) or {}
            side["lods"] = out_names
            if side_path.exists():
                _save_json(side_path, side)
            man = _load_json(MANIFEST) or {}
            entry = next((x for x in man.get("items") or [] if x.get("code") == code), None)
            if entry is not None:
                entry["lods"] = out_names
                _save_json(MANIFEST, man)
            LIB._mtime = 0.0
            results.append({"code": code, "lods": out_names, "created": len(made)})
    return {"processed": sum(1 for r in results if "lods" in r), "results": results}


# ----------------------------------------------------------------- textures
def texture_audit() -> dict:
    """Texture weight, resolution histogram, per-asset counts and oversized files."""
    tex_root = ROOT / "_textures"
    by_ext: dict[str, int] = {}
    by_res: dict[str, int] = {}
    total = 0
    count = 0
    derived = 0
    for dp, _, fs in os.walk(tex_root):
        for f in fs:
            p = Path(dp) / f
            try:
                size = p.stat().st_size
            except OSError:
                continue
            total += size
            count += 1
            if "_derived" in p.parts:
                derived += 1
                continue
            by_ext[p.suffix.lower()] = by_ext.get(p.suffix.lower(), 0) + 1
            if p.suffix.lower() == ".png":
                from .library import png_size

                dim = png_size(p)
                if dim:
                    key = f"{dim[0]}x{dim[1]}"
                    by_res[key] = by_res.get(key, 0) + 1
    heavy = []
    for it in LIB.items():
        n = it.get("n_tex") or 0
        if n >= 25:
            heavy.append({"code": it["code"], "slug": it.get("slug"), "n_tex": n})
    heavy.sort(key=lambda r: -r["n_tex"])
    return {
        "files": count,
        "derived_files": derived,
        "bytes": total,
        "mb": round(total / 1e6, 1),
        "by_extension": by_ext,
        "png_resolutions": dict(sorted(by_res.items(), key=lambda kv: -kv[1])[:12]),
        "heavy_assets": heavy[:20],
        "suggestion": (
            "props/vegetation cards can live at 512; keep 2048 only for hero architecture; "
            "run texture_downscale(codes, max_px) and switch the referencing assets to the "
            "derived copies when a lower budget is needed."
        ),
    }


def downscale_textures(codes: list[str] | None = None, max_px: int = 1024) -> dict:
    """Write resized copies under `_textures/_derived/<max_px>/` (originals untouched)."""
    try:
        from PIL import Image
    except ImportError:
        return {"error": "Pillow is not installed in this environment"}

    if codes:
        rels: set[str] = set()
        for code in codes:
            it = LIB.get(code)
            if it:
                rels.update(str(r) for r in (it.get("textures") or []))
    else:
        rels = set()
        for dp, _, fs in os.walk(ROOT / "_textures"):
            for f in fs:
                p = Path(dp) / f
                if "_derived" in p.parts:
                    continue
                rels.add(str(p.relative_to(ROOT)).replace(os.sep, "/"))

    made, skipped, failed = [], 0, []
    for rel in sorted(rels):
        src = ROOT / rel.replace("/", os.sep)
        if not src.exists():
            continue
        try:
            with Image.open(src) as im:
                w, h = im.size
                if max(w, h) <= max_px:
                    skipped += 1
                    continue
                scale = max_px / max(w, h)
                out = im.resize((max(1, round(w * scale)), max(1, round(h * scale))), Image.LANCZOS)
                dst = ROOT / "_textures" / "_derived" / str(max_px) / Path(rel).relative_to("_textures")
                dst.parent.mkdir(parents=True, exist_ok=True)
                out.save(dst)
                made.append({"src": rel, "dst": str(dst.relative_to(ROOT)).replace(os.sep, "/"),
                             "from": f"{w}x{h}", "to": f"{out.size[0]}x{out.size[1]}"})
        except Exception as exc:  # noqa: BLE001 - report, do not abort the batch
            failed.append({"src": rel, "error": str(exc)[:120]})
    return {"max_px": max_px, "written": len(made), "skipped_small": skipped,
            "failed": failed[:10], "files": made[:40]}
