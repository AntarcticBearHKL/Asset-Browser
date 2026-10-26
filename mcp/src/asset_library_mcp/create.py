"""Authoring-side tools: register new assets, AI tagging, taxonomy.

The heavy lifting lives in the library itself (`<root>/_Catalog/tools/`), so the
Blender add-on and this MCP server run exactly the same code:

  * `library_index.py`  - code allocation, validation, manifest writing (single writer)
  * `deepseek_tag.py`   - DeepSeek auto-tagging (shared by Blender / MCP / app)

This module only adapts them to the MCP tool surface.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

from .library import LIB, ROOT

TOOLS = ROOT / "_Catalog" / "tools"


def _load(name: str):
    """Import `<root>/_Catalog/tools/<name>.py` (no packaging, one source of truth)."""
    cached = sys.modules.get(f"assetlib_tools_{name}")
    if cached is not None:
        return cached
    path = TOOLS / f"{name}.py"
    if not path.exists():
        raise FileNotFoundError(f"missing library tool: {path}")
    spec = importlib.util.spec_from_file_location(f"assetlib_tools_{name}", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    sys.modules[f"assetlib_tools_{name}"] = module
    return module


def index():
    return _load("library_index")


def tagger():
    return _load("deepseek_tag")


# ------------------------------------------------------------------- taxonomy
def taxonomy() -> dict:
    return index().taxonomy()


def next_code(l1: str = "") -> dict:
    try:
        return {"ok": True, "code": index().allocate_code(l1), "l1": l1}
    except (ValueError, TimeoutError) as exc:
        return {"ok": False, "error": str(exc)}


# --------------------------------------------------------------------- create
def create_asset(asset_rel: str, l1: str = "", l2: str = "", tags=None,
                 description: str = "", code: str = "", dry_run: bool = False) -> dict:
    """Register an asset folder that already contains its `<slug>.json` sidecar.

    `asset_rel` is library-relative, e.g. `Models/Prop/Container/old_well`.
    """
    rel = (asset_rel or "").strip().replace("\\", "/").strip("/")
    if not rel:
        return {"ok": False, "error": "asset_rel is required"}

    folder = ROOT / rel.replace("/", "\\")
    slug = rel.rsplit("/", 1)[-1]
    sidecar = folder / f"{slug}.json"
    if not sidecar.exists():
        return {"ok": False, "error": f"sidecar not found: {sidecar}"}

    try:
        doc = json.loads(sidecar.read_text(encoding="utf-8-sig"))
    except json.JSONDecodeError as exc:
        return {"ok": False, "error": f"sidecar is not valid json: {exc}"}

    if l1:
        doc["l1"] = l1
        doc["category"] = f"{l1}/{l2}" if l2 else l1
    if l2:
        doc["l2"] = l2
        doc["category"] = f"{doc.get('l1', '')}/{l2}"
    if tags is not None:
        doc["tags"] = index().normalize_tags(tags)
    if description:
        doc["description"] = description

    index()._write_json(sidecar, doc)          # normalise the metadata first
    result = index().register(sidecar, code=code, dry_run=dry_run)
    result["asset"] = rel
    if result.get("ok"):
        LIB._mtime = 0.0                       # force the search index to reload
    return result


# ---------------------------------------------------------------- ai tagging
def ai_status() -> dict:
    try:
        st = tagger().status()
    except Exception as exc:                   # noqa: BLE001 - report, never crash a tool
        return {"ok": False, "error": str(exc)}
    st["ok"] = True
    return st


def auto_tag(code: str, apply: bool = False, overwrite: bool = False) -> dict:
    """Ask DeepSeek to classify one asset from its preview image.

    `apply=False` only returns the suggestion (a diff the caller can show);
    `apply=True` writes category/tags/description through the single writer.
    """
    it = LIB.get(code)
    if not it:
        return {"ok": False, "error": f"unknown asset: {code}"}

    preview = LIB.preview_path(it)
    if not preview or not Path(preview).exists():
        return {"ok": False, "error": f"{code} has no preview image - run preview_regenerate first"}

    side = LIB.sidecar(it) or {}
    context = {
        "slug": it.get("slug"),
        "code": it.get("code"),
        "kind": it.get("kind"),
        "l1": it.get("l1"),
        "l2": it.get("l2"),
        "tags": it.get("tags") or [],
        "description": side.get("description") or "",
        "dimensions_m": it.get("dimensions_m"),
        "tri_count": it.get("tri_count"),
        "materials": [m.get("name") for m in (side.get("materials") or []) if m.get("name")],
    }

    result = tagger().tag(preview, context)
    if not result.get("ok"):
        return result

    if apply:
        tags = result["tags"]
        if not overwrite:
            tags = index().normalize_tags(list(it.get("tags") or []) + list(tags))[: index().MAX_TAGS]
        saved = index().update_metadata(code, tags=tags,
                                        description=result.get("description") or None,
                                        l1=result.get("l1") or None, l2=result.get("l2") or None)
        LIB._mtime = 0.0
        result["applied"] = saved
        result["tags"] = tags

    result.pop("raw", None)
    result["code"] = it.get("code")
    result["slug"] = it.get("slug")
    result["preview"] = str(preview)
    return result


def auto_tag_batch(codes: list[str] | None = None, limit: int = 20, apply: bool = False,
                   only_untagged: bool = True) -> dict:
    """Tag many assets (default: only those with fewer than 2 tags)."""
    rows = []
    if codes:
        selected = [LIB.get(c) for c in codes]
    else:
        selected = []
        for it in LIB.items():
            if it.get("kind") != "model":
                continue
            if only_untagged and len(it.get("tags") or []) >= 2:
                continue
            selected.append(it)

    for it in [x for x in selected if x][: max(1, limit)]:
        res = auto_tag(it["code"], apply=apply)
        rows.append({k: res.get(k) for k in ("code", "slug", "ok", "l1", "l2", "tags",
                                             "description", "confidence", "error")})
    ok = sum(1 for r in rows if r.get("ok"))
    return {"requested": len(rows), "tagged": ok, "applied": bool(apply), "results": rows}


# ------------------------------------------------------------------ revisions
def revisions(code: str) -> dict:
    """Snapshot history of one asset (newest first)."""
    try:
        return index().list_revisions(code)
    except Exception as exc:                   # noqa: BLE001 - report, never crash a tool
        return {"ok": False, "error": str(exc)}


def restore(code: str, revision: str) -> dict:
    """Roll one asset back to a snapshot; the current state is snapshotted first."""
    try:
        result = index().restore_revision(code, revision)
    except Exception as exc:                   # noqa: BLE001
        return {"ok": False, "error": str(exc)}
    if result.get("ok"):
        LIB._mtime = 0.0
    return result
