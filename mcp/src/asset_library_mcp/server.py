"""MCP tool surface for the asset library.

The tools are grouped by the job an agent actually does:

* discover  - search_assets, library_facets, library_stats, find_similar
* understand- get_asset, get_preview, list_textures, find_texture_usage, list_clips, get_paths
* transfer  - unity_status, unity_import, unity_import_scene, unity_import_rig,
              unity_import_batch (staged, one transaction), unity_sync_previews
* workflow  - cart_list / cart_add / cart_remove / cart_clear / cart_export
* hygiene   - library_qa
"""

from __future__ import annotations

import base64
from typing import Any

from mcp import types
from mcp.server.fastmcp import FastMCP

from .library import LIB, ROOT
from . import create, maintenance, unity_bridge

mcp = FastMCP("asset-library")


# ------------------------------------------------------------------ discover
@mcp.tool()
def search_assets(
    query: str = "",
    kind: str = "",
    l1: str = "",
    l2: str = "",
    tags: list[str] | None = None,
    min_tris: int | None = None,
    max_tris: int | None = None,
    min_dim: float | None = None,
    max_dim: float | None = None,
    lod_min: int = 0,
    textured: bool | None = None,
    sort: str = "code",
    limit: int = 50,
    offset: int = 0,
) -> dict:
    """Search the local 3D asset library.

    `query` matches code, slug, original name, category and tags (case-insensitive).
    Narrow with `kind` (model/material/rig), `l1`/`l2` category levels, `tags`
    (all must match), a triangle range (`min_tris`/`max_tris`), a longest-side
    range in metres (`min_dim`/`max_dim`), `lod_min`, and `textured`.
    `sort` is one of code, slug, tris_asc, tris_desc, name. Rows are compact;
    call get_asset for a full record.
    """
    rows = LIB.search(
        query=query, kind=kind, l1=l1, l2=l2, tags=tags, min_tris=min_tris, max_tris=max_tris,
        min_dim=min_dim, max_dim=max_dim, lod_min=lod_min, textured=textured, sort=sort,
        limit=limit, offset=offset,
    )
    total = len(
        LIB.search(
            query=query, kind=kind, l1=l1, l2=l2, tags=tags, min_tris=min_tris, max_tris=max_tris,
            min_dim=min_dim, max_dim=max_dim, lod_min=lod_min, textured=textured, sort=sort,
            limit=100000, offset=0,
        )
    )
    return {"total": total, "returned": len(rows), "offset": offset, "items": rows}


@mcp.tool()
def library_facets() -> dict:
    """The filter values that exist (kinds, l1/l2 categories, tags, sort keys)."""
    return LIB.facets()


@mcp.tool()
def library_stats() -> dict:
    """Library totals: item counts, triangles, LOD coverage, texture weight, packs."""
    return LIB.stats()


@mcp.tool()
def find_similar(code: str, limit: int = 8) -> dict:
    """Assets that look like this one (same category, shared tags)."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    return {"code": it["code"], "similar": LIB.similar(it, limit)}


# ---------------------------------------------------------------- understand
@mcp.tool()
def get_asset(code: str) -> dict:
    """Full record for one asset: geometry, materials, textures, clips, paths."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    return LIB.row(it, detail=True)


@mcp.tool()
def get_preview(code: str) -> list[types.ContentBlock]:
    """The asset's 512x512 preview image, so you can look at it."""
    it = LIB.get(code)
    if not it:
        return [types.TextContent(type="text", text=f"unknown asset {code}")]
    p = LIB.preview_path(it)
    if not p:
        return [types.TextContent(type="text", text=f"{code} has no preview on disk")]
    data = base64.b64encode(p.read_bytes()).decode("ascii")
    return [
        types.ImageContent(type="image", data=data, mimeType="image/png"),
        types.TextContent(type="text", text=f"{code} preview: {p}"),
    ]


@mcp.tool()
def list_textures(code: str) -> dict:
    """Texture files this asset references, with resolution and size on disk."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    return {"code": it["code"], "textures": LIB.texture_paths(it)}


@mcp.tool()
def find_texture_usage(texture: str) -> dict:
    """Which assets reference a texture (accepts a path fragment or file name)."""
    needle = texture.replace("\\", "/").lower()
    hits = []
    for it in LIB.items():
        for t in it.get("textures") or []:
            if needle in str(t).replace("\\", "/").lower():
                hits.append({"code": it["code"], "slug": it.get("slug"), "texture": t, "kind": it.get("kind")})
                break
    return {"query": texture, "count": len(hits), "assets": hits[:100]}


@mcp.tool()
def list_clips(code: str) -> dict:
    """Animation clips of a rig, with frames, fps and duration."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    clips = LIB.clips(it)
    return {"code": it["code"], "slug": it["slug"], "count": len(clips), "clips": clips}


@mcp.tool()
def get_paths(code: str) -> dict:
    """Absolute paths (folder, FBX/LODs, preview, sidecar) — for tools that read files themselves."""
    it = LIB.get(code)
    if not it:
        return {"error": f"unknown asset {code}"}
    d = LIB.asset_dir(it)
    return {
        "code": it["code"],
        "kind": it.get("kind"),
        "dir": str(d),
        "fbx": [str(p) for p in LIB.fbx_paths(it)],
        "sidecar": str(d / f"{it.get('slug')}.json"),
        "preview": str(LIB.preview_path(it) or ""),
        "glb": str(ROOT / str(it["glb"]).replace("/", "\\")) if it.get("glb") else "",
        "clips_file": str(ROOT / str(it["clipsFile"]).replace("/", "\\")) if it.get("clipsFile") else "",
    }


# ------------------------------------------------------------------ transfer
@mcp.tool()
def unity_status() -> dict:
    """Whether the Unity editor MCP is up, plus project path and active scene."""
    try:
        return unity_bridge.status()
    except unity_bridge.UnityError as exc:
        return {"online": False, "error": str(exc)}


@mcp.tool()
def unity_import(codes: list[str], spacing: float = 2.0, columns: int = 5, rot_y: float = 0.0) -> dict:
    """Import assets into the open Unity scene.

    Copies each asset's FBX and textures into `Assets/AssetLib/Imported/<code>_<slug>/`,
    builds URP/Lit materials from the library's sidecar data (base colour, normal,
    metallic, smoothness, transparency), then instantiates the models in a grid
    spaced `spacing` metres apart, `columns` per row. Returns what Unity reported.
    """
    try:
        return unity_bridge.import_assets(codes, spacing=spacing, columns=columns, rot_y=rot_y)
    except unity_bridge.UnityError as exc:
        return {"error": str(exc)}


# ------------------------------------------------------------------ workflow
@mcp.tool()
def cart_list() -> dict:
    """The cart shared with the desktop app (persisted in its store.json)."""
    codes = LIB.cart()
    return {"count": len(codes), "codes": codes, "assets": [LIB.row(LIB.get(c)) for c in codes if LIB.get(c)]}


@mcp.tool()
def cart_add(codes: list[str]) -> dict:
    """Add asset codes to the shared cart."""
    return {"cart": LIB.cart_set(LIB.cart() + codes)}


@mcp.tool()
def cart_remove(codes: list[str]) -> dict:
    """Remove asset codes from the shared cart."""
    drop = {c.lower() for c in codes}
    return {"cart": LIB.cart_set([c for c in LIB.cart() if c.lower() not in drop])}


@mcp.tool()
def cart_clear() -> dict:
    """Empty the shared cart."""
    return {"cart": LIB.cart_set([])}


@mcp.tool()
def cart_export() -> dict:
    """The cart as a plain code list, ready to paste into a script or a ticket."""
    codes = LIB.cart()
    return {"count": len(codes), "text": "\n".join(codes), "codes": codes}


# ------------------------------------------------------------------- hygiene
@mcp.tool()
def library_qa() -> dict:
    """Library health report: missing previews/textures, null dimensions,
    high-poly assets without LODs, oversized textures, rigs without clips,
    and whether the manifest is older than the newest asset edit."""
    return LIB.qa()


# ------------------------------------------------------------ library upkeep
@mcp.tool()
def asset_update(code: str, tags: list[str] | None = None, l1: str = "", l2: str = "",
                 description: str = "") -> dict:
    """Edit an asset's tags, category (l1/l2) or description.

    Writes the sidecar JSON and the manifest together so the app and the library
    stay in sync. Only the fields you pass change.
    """
    return maintenance.update_asset(code, tags=tags, l1=l1 or None, l2=l2 or None,
                                    description=description or None)


@mcp.tool()
def preview_regenerate(codes: list[str], res: int = 512) -> dict:
    """Re-render library previews with headless Blender (clay material, auto
    camera, idle action). The previous PNG is backed up under %TEMP%."""
    return maintenance.regenerate_preview(codes, res=res)


@mcp.tool()
def lod_generate(codes: list[str], ratios: list[float] | None = None) -> dict:
    """Decimate a model's base FBX into `_LOD1/_LOD2` files and record them in
    the sidecar + manifest. Default ratios: 0.5 then 0.25."""
    return maintenance.generate_lods(codes, ratios=tuple(ratios or (0.5, 0.25)))


@mcp.tool()
def texture_audit() -> dict:
    """Texture weight and resolution breakdown, heavy assets, downscale advice."""
    return maintenance.texture_audit()


@mcp.tool()
def texture_downscale(codes: list[str] | None = None, max_px: int = 1024) -> dict:
    """Write resized copies of the textures used by `codes` (or the whole
    library) under `_textures/_derived/<max_px>/`. Originals are never touched."""
    return maintenance.downscale_textures(codes, max_px=max_px)


@mcp.tool()
def find_by_description(text: str, limit: int = 20) -> dict:
    """Rank assets against a free-form description ("wooden tavern roof",
    "snowy pine", "kung fu kick"), expanding words through a synonym map and
    scoring slug / original name / tags / category / description hits."""
    hits = LIB.find_by_description(text, limit=limit)
    return {"query": text, "count": len(hits), "items": hits}


# ------------------------------------------------------------ Unity: sets/rigs
@mcp.tool()
def unity_import_scene(pack: str = "", category: str = "", kind: str = "model", limit: int = 24,
                       layout: str = "grid", spacing: float = 2.0, columns: int = 5) -> dict:
    """Import a whole set into Unity at once: everything from a source pack
    (e.g. "Modular Wooden Set") or a category (e.g. "Architecture/Fence"),
    laid out as a `grid`, a `row`, or a `ring`."""
    try:
        return unity_bridge.import_scene(pack=pack, category=category, kind=kind, limit=limit,
                                         layout=layout, spacing=spacing, columns=columns)
    except unity_bridge.UnityError as exc:
        return {"error": str(exc)}


@mcp.tool()
def unity_import_rig(code: str, clips: list[str] | None = None, x: float = 0.0, y: float = 0.0,
                     z: float = 0.0) -> dict:
    """Import a rig into Unity as a Humanoid avatar plus an Animator whose
    controller holds the rig's animation clips (optionally only `clips`)."""
    try:
        return unity_bridge.import_rig(code, clips=clips, x=x, y=y, z=z)
    except unity_bridge.UnityError as exc:
        return {"error": str(exc)}


@mcp.tool()
def unity_import_batch(codes: list[str] | None = None, pack: str = "", category: str = "",
                       cart: bool = False, force: bool = False) -> dict:
    """Stage a whole set for Unity, then import it in ONE transaction.

    All FBX/LOD files, textures, sidecars and previews of every selected asset are copied into
    `Assets/AssetLib/<library path>` first, then Unity imports the prepared tree in a single
    AssetDatabase transaction (materials, prefabs, ledger) instead of analysing asset by asset.

    Select with `codes`, with `cart=True` (the shared cart), or with `pack` / `category`.
    """
    selected: list[str] = [c for c in (codes or []) if c]
    if not selected and cart:
        selected = LIB.cart()
    if not selected and (pack or category):
        selected = [r["code"] for r in LIB.resolve(pack=pack, category=category, kind="model", limit=0)]
    if not selected:
        return {"error": "nothing selected (pass codes, cart=True, or pack/category)"}

    try:
        return unity_bridge.bulk_import(selected, force=force)
    except unity_bridge.UnityError as exc:
        return {"error": str(exc)}


@mcp.tool()
def unity_sync_previews(force: bool = False) -> dict:
    """One-click preview refresh: re-pull every imported model's `<slug>.png`
    from the library into Assets/AssetLib and reimport the changed textures."""
    try:
        return unity_bridge.sync_previews(force=force)
    except unity_bridge.UnityError as exc:
        return {"error": str(exc)}


# ------------------------------------------------------------------ authoring
@mcp.tool()
def library_taxonomy() -> dict:
    """The canonical L1/L2 category lists (what the Blender create panel and the
    tagger both use)."""
    try:
        return {"taxonomy": create.taxonomy()}
    except FileNotFoundError as exc:
        return {"error": str(exc)}


@mcp.tool()
def asset_next_code(l1: str = "") -> dict:
    """Next free asset code (`AU-0001` series for authored assets)."""
    return create.next_code(l1)


@mcp.tool()
def asset_create(asset_rel: str, l1: str = "", l2: str = "", tags: list[str] | None = None,
                 description: str = "", code: str = "", dry_run: bool = False) -> dict:
    """Register a new asset folder that already holds its `<slug>.json` sidecar
    (e.g. written by the Blender add-on), assigning a code and updating the
    manifest. `asset_rel` is library-relative, e.g. `Models/Prop/Container/old_well`."""
    try:
        return create.create_asset(asset_rel, l1=l1, l2=l2, tags=tags, description=description,
                                   code=code, dry_run=dry_run)
    except (ValueError, TimeoutError) as exc:
        return {"error": str(exc)}


@mcp.tool()
def ai_status() -> dict:
    """Whether a DeepSeek key is configured (Settings → AI · DeepSeek), and which model."""
    return create.ai_status()


@mcp.tool()
def asset_auto_tag(code: str, apply: bool = False, overwrite: bool = False) -> dict:
    """Send one asset's preview to `deepseek-flash` and return suggested
    category / tags / description. `apply=True` writes them (merged with the
    existing tags unless `overwrite`)."""
    try:
        return create.auto_tag(code, apply=apply, overwrite=overwrite)
    except (ValueError, TimeoutError) as exc:
        return {"error": str(exc)}


@mcp.tool()
def asset_auto_tag_batch(codes: list[str] | None = None, limit: int = 20, apply: bool = False,
                         only_untagged: bool = True) -> dict:
    """Tag a set of assets (default: models with fewer than 2 tags)."""
    try:
        return create.auto_tag_batch(codes, limit=limit, apply=apply, only_untagged=only_untagged)
    except (ValueError, TimeoutError) as exc:
        return {"error": str(exc)}


@mcp.tool()
def asset_revisions(code: str) -> dict:
    """Snapshot history of one asset (newest first). Snapshots are written by
    `asset_create` / the Blender save, keeping the last 10 per asset."""
    return create.revisions(code)


@mcp.tool()
def asset_restore(code: str, revision: str) -> dict:
    """Roll one asset back to a snapshot id from `asset_revisions`
    (the current state is snapshotted first, so the restore is undoable)."""
    return create.restore(code, revision)
