"""Read-only access to the 3D asset library: manifest, sidecars, textures, clips, cart."""

from __future__ import annotations

import json
import os
import struct
from pathlib import Path
from typing import Any, Iterable

ROOT = Path(os.environ.get("ASSET_LIBRARY_ROOT", r"D:\3D Resource"))
CATALOG = ROOT / "_Catalog"
MANIFEST = CATALOG / "manifest.json"
TARGETS = ROOT / "_targets"
STORE = Path(os.environ.get("APPDATA", "")) / "asset-library-desktop" / "store.json"


# --------------------------------------------------------------------------- io
def _load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError):
        return None


def png_size(path: Path) -> tuple[int, int] | None:
    try:
        with open(path, "rb") as fh:
            head = fh.read(24)
        if head[:8] == b"\x89PNG\r\n\x1a\n" and head[12:16] == b"IHDR":
            return struct.unpack(">II", head[16:24])
    except OSError:
        pass
    return None


class Library:
    """The manifest, loaded once and reloaded when the file changes."""

    def __init__(self) -> None:
        self._items: list[dict] | None = None
        self._by_code: dict[str, dict] = {}
        self._by_slug: dict[str, dict] = {}
        self._mtime: float = 0.0

    # ---- manifest
    def items(self) -> list[dict]:
        try:
            mtime = MANIFEST.stat().st_mtime
        except OSError:
            return []
        if self._items is None or mtime != self._mtime:
            doc = _load_json(MANIFEST) or {}
            self._items = doc.get("items") or []
            self._by_code = {i.get("code", ""): i for i in self._items}
            self._by_slug = {i.get("slug", ""): i for i in self._items}
            self._mtime = mtime
        return self._items

    def get(self, code_or_slug: str) -> dict | None:
        self.items()
        return self._by_code.get(code_or_slug) or self._by_slug.get(code_or_slug)

    # ---- paths
    def asset_dir(self, it: dict) -> Path:
        kind = it.get("kind")
        slug = it.get("slug", "")
        if kind == "model":
            return ROOT / "Models" / it.get("category", "").replace("/", os.sep) / slug
        if kind == "material":
            return ROOT / "Materials" / it.get("category", "").replace("/", os.sep) / slug
        if kind == "rig":
            return ROOT / "Rig" / it.get("l1", "Humanoid") / slug
        return ROOT / str(slug)

    def sidecar(self, it: dict) -> dict:
        return _load_json(self.asset_dir(it) / f"{it.get('slug')}.json") or {}

    def preview_path(self, it: dict) -> Path | None:
        rel = it.get("preview")
        if not rel:
            return None
        p = ROOT / str(rel).replace("/", os.sep)
        return p if p.exists() else None

    def fbx_paths(self, it: dict) -> list[Path]:
        """Every LOD/source FBX on disk for this asset (best first)."""
        d = self.asset_dir(it)
        lods = it.get("lods") or []
        out = [d / str(name) for name in lods]
        if not out and it.get("kind") == "rig":
            out = [d / f"{it.get('slug')}.fbx"]
        return [p for p in out if p.exists()]

    def texture_paths(self, it: dict) -> list[dict]:
        out = []
        for rel in it.get("textures") or []:
            p = ROOT / str(rel).replace("/", os.sep)
            size = png_size(p) if p.suffix.lower() == ".png" else None
            out.append(
                {
                    "path": str(p),
                    "rel": str(rel),
                    "exists": p.exists(),
                    "bytes": p.stat().st_size if p.exists() else 0,
                    "width": size[0] if size else None,
                    "height": size[1] if size else None,
                }
            )
        return out

    # ---- rows
    def row(self, it: dict, detail: bool = False) -> dict:
        dims = it.get("dimensions_m") or {}
        tex = self.texture_paths(it) if detail else []
        out = {
            "code": it.get("code"),
            "slug": it.get("slug"),
            "kind": it.get("kind"),
            "category": it.get("category"),
            "name_original": it.get("name_original"),
            "tri_count": it.get("tri_count"),
            "lods": len(it.get("lods") or []),
            "n_tex": it.get("n_tex") or it.get("n_channels") or 0,
            "dimensions_m": dims or None,
            "tags": it.get("tags") or [],
            "has_preview": bool(self.preview_path(it)),
        }
        if detail:
            side = self.sidecar(it)
            out.update(
                {
                    "description": side.get("description"),
                    "geometry": side.get("geometry"),
                    "materials": side.get("materials") or [],
                    "textures": tex,
                    "textures_by_kind": side.get("textures_by_kind") or {},
                    "source_pack": side.get("source_pack"),
                    "untextured": side.get("untextured"),
                    "lods": it.get("lods") or [],
                    "clip_count": it.get("clipCount"),
                    "clips": self.clips(it) if it.get("kind") == "rig" else [],
                    "bone_count": it.get("boneCount"),
                    "retargetable": it.get("retargetable"),
                    "paths": {
                        "dir": str(self.asset_dir(it)),
                        "fbx": [str(p) for p in self.fbx_paths(it)],
                        "preview": str(self.preview_path(it) or ""),
                    },
                }
            )
        return out

    # ---- search
    def search(self, **kw: Any) -> list[dict]:
        q = (kw.get("query") or "").strip().lower()
        kind = kw.get("kind")
        l1 = (kw.get("l1") or "").strip().lower()
        l2 = (kw.get("l2") or "").strip().lower()
        tags = [t.lower() for t in (kw.get("tags") or [])]
        min_tris = kw.get("min_tris")
        max_tris = kw.get("max_tris")
        min_dim = kw.get("min_dim")
        max_dim = kw.get("max_dim")
        lod_min = kw.get("lod_min") or 0
        textured = kw.get("textured")
        sort = kw.get("sort") or "code"
        limit = max(1, min(int(kw.get("limit") or 50), 500))
        offset = max(0, int(kw.get("offset") or 0))

        rows: list[dict] = []
        for it in self.items():
            if kind and it.get("kind") != kind:
                continue
            if l1 and str(it.get("l1", "")).lower() != l1:
                continue
            if l2 and str(it.get("l2", "")).lower() != l2:
                continue
            if tags:
                have = {t.lower() for t in (it.get("tags") or [])}
                if not all(t in have for t in tags):
                    continue
            tris = it.get("tri_count") or 0
            if min_tris is not None and tris < min_tris:
                continue
            if max_tris is not None and tris > max_tris:
                continue
            dims = it.get("dimensions_m") or {}
            longest = max((float(v) for v in dims.values()), default=0.0) if dims else 0.0
            if min_dim is not None and longest < min_dim:
                continue
            if max_dim is not None and longest > max_dim:
                continue
            if lod_min and len(it.get("lods") or []) < lod_min:
                continue
            if textured is True and not (it.get("n_tex") or 0):
                continue
            if textured is False and (it.get("n_tex") or 0):
                continue
            if q:
                hay = " ".join(
                    str(x)
                    for x in (
                        it.get("code"),
                        it.get("slug"),
                        it.get("name_original"),
                        it.get("category"),
                        " ".join(it.get("tags") or []),
                    )
                ).lower()
                if q not in hay:
                    continue
            rows.append(self.row(it))

        key = {
            "code": lambda r: r["code"] or "",
            "slug": lambda r: r["slug"] or "",
            "tris_asc": lambda r: r["tri_count"] or 0,
            "tris_desc": lambda r: -(r["tri_count"] or 0),
            "name": lambda r: (r["name_original"] or "").lower(),
        }.get(sort, lambda r: r["code"] or "")
        rows.sort(key=key)
        return rows[offset : offset + limit]

    def similar(self, it: dict, limit: int = 8) -> list[dict]:
        slug = it.get("slug")
        cat = it.get("category")
        tags = set(it.get("tags") or [])
        scored = []
        for other in self.items():
            if other.get("code") == it.get("code"):
                continue
            score = 0
            if other.get("category") == cat:
                score += 3
            score += len(tags & set(other.get("tags") or []))
            if score:
                scored.append((score, other))
        scored.sort(key=lambda kv: (-kv[0], kv[1].get("code") or ""))
        return [self.row(o) for _, o in scored[:limit]]

    # ---- aggregate
    def stats(self) -> dict:
        items = self.items()
        by_kind: dict[str, int] = {}
        by_l1: dict[str, int] = {}
        by_pack: dict[str, int] = {}
        tris = 0
        lod_hist: dict[int, int] = {}
        for it in items:
            by_kind[it.get("kind", "?")] = by_kind.get(it.get("kind", "?"), 0) + 1
            by_l1[it.get("l1", "?")] = by_l1.get(it.get("l1", "?"), 0) + 1
            pack = self.sidecar(it).get("source_pack") or "(none)"
            by_pack[pack] = by_pack.get(pack, 0) + 1
            tris += it.get("tri_count") or 0
            n = len(it.get("lods") or [])
            lod_hist[n] = lod_hist.get(n, 0) + 1
        tex_bytes = 0
        for dp, _, fs in os.walk(ROOT / "_textures"):
            for f in fs:
                try:
                    tex_bytes += (Path(dp) / f).stat().st_size
                except OSError:
                    pass
        return {
            "items": len(items),
            "by_kind": by_kind,
            "by_l1": dict(sorted(by_l1.items(), key=lambda kv: -kv[1])),
            "by_pack": dict(sorted(by_pack.items(), key=lambda kv: -kv[1])[:20]),
            "triangles_total": tris,
            "lod_histogram": dict(sorted(lod_hist.items())),
            "texture_bytes": tex_bytes,
            "generated_at": (_load_json(MANIFEST) or {}).get("generatedAt"),
        }

    def facets(self) -> dict:
        l1: dict[str, int] = {}
        l2: dict[str, dict[str, int]] = {}
        tags: dict[str, int] = {}
        for it in self.items():
            a = it.get("l1") or "?"
            l1[a] = l1.get(a, 0) + 1
            b = it.get("l2") or "?"
            l2.setdefault(a, {})
            l2[a][b] = l2[a].get(b, 0) + 1
            for t in it.get("tags") or []:
                tags[t] = tags.get(t, 0) + 1
        return {
            "kind": ["model", "material", "rig"],
            "l1": dict(sorted(l1.items(), key=lambda kv: -kv[1])),
            "l2": {k: dict(sorted(v.items(), key=lambda kv: -kv[1])) for k, v in sorted(l2.items())},
            "tags": dict(sorted(tags.items(), key=lambda kv: -kv[1])),
            "sorts": ["code", "slug", "tris_asc", "tris_desc", "name"],
        }

    # ---- rigs
    def clips(self, it: dict) -> list[dict]:
        if it.get("kind") != "rig":
            return []
        rel = it.get("clipsFile")
        doc = None
        if rel:
            doc = _load_json(ROOT / str(rel).replace("/", os.sep))
        if doc is None:
            doc = _load_json(self.asset_dir(it) / f"{it.get('slug')}.clips.json")
        out = []
        for c in (doc or {}).get("clips") or []:
            frames = c.get("frames") or 0
            fps = c.get("fps") or 30
            out.append(
                {
                    "name": c.get("name"),
                    "frames": frames,
                    "fps": fps,
                    "seconds": round(frames / fps, 3) if fps else 0.0,
                }
            )
        return out

    # ---- cart (shared with the desktop app)
    def cart(self) -> list[str]:
        doc = _load_json(STORE) or {}
        return list(doc.get("cart") or [])

    def cart_set(self, codes: Iterable[str]) -> list[str]:
        doc = _load_json(STORE) or {}
        doc["cart"] = sorted({c for c in codes if self.get(c)})
        STORE.parent.mkdir(parents=True, exist_ok=True)
        STORE.write_text(json.dumps(doc, indent=2), encoding="utf-8")
        return doc["cart"]

    # ---- semantic-ish lookup
    def find_by_description(self, text: str, limit: int = 20) -> list[dict]:
        """Rank assets against a free-form description.

        Not an embedding search: it expands the words through a synonym map and
        scores slug / original name / tags / category / description hits, so
        "wooden tavern roof" reaches the thatch and wooden-beam assets.
        """
        import re

        words = [w for w in re.split(r"[^a-z0-9]+", text.lower()) if len(w) > 1]
        if not words:
            return []
        expanded: dict[str, int] = {}
        for w in words:
            expanded[w] = expanded.get(w, 0) + 3
            for syn in SYNONYMS.get(w, ()):
                expanded[syn] = expanded.get(syn, 0) + 2
            if w.endswith("s"):
                expanded[w[:-1]] = expanded.get(w[:-1], 0) + 2
        scored: list[tuple[int, dict, list[str]]] = []
        for it in self.items():
            side = self.sidecar(it)
            slug = str(it.get("slug", "")).lower()
            name = str(it.get("name_original", "")).lower()
            cat = str(it.get("category", "")).lower()
            tags = " ".join(it.get("tags") or []).lower()
            desc = str(side.get("description", "")).lower()
            score = 0
            hits: list[str] = []
            for term, weight in expanded.items():
                if term in slug:
                    score += 5 * weight
                    hits.append(term)
                elif term in name:
                    score += 4 * weight
                    hits.append(term)
                elif term in tags:
                    score += 3 * weight
                    hits.append(term)
                elif term in cat:
                    score += 2 * weight
                    hits.append(term)
                elif term in desc:
                    score += weight
                    hits.append(term)
            if score:
                scored.append((score, it, sorted(set(hits))))
        scored.sort(key=lambda kv: (-kv[0], kv[1].get("code") or ""))
        out = []
        for score, it, hits in scored[:limit]:
            row = self.row(it)
            row["score"] = score
            row["matched"] = hits
            out.append(row)
        return out

    def resolve(self, pack: str = "", category: str = "", kind: str = "model", limit: int = 60) -> list[dict]:
        """Assets of a source pack and/or category — the unit a kit import works in."""
        want_pack = (pack or "").lower()
        want_cat = (category or "").lower()
        out = []
        for it in self.items():
            if kind and it.get("kind") != kind:
                continue
            if want_cat and want_cat not in str(it.get("category", "")).lower():
                continue
            if want_pack:
                side = self.sidecar(it)
                if want_pack not in str(side.get("source_pack", "")).lower():
                    continue
            out.append(self.row(it))
            if len(out) >= limit:
                break
        return out

    # ---- QA
    def qa(self) -> dict:
        items = self.items()
        missing_preview, missing_tex, null_dims = [], [], []
        hi_single_lod, many_tex, big_tex = [], [], []
        stale_rigs = []
        for it in items:
            code = it.get("code")
            if not self.preview_path(it):
                missing_preview.append(code)
            for t in self.texture_paths(it):
                if not t["exists"]:
                    missing_tex.append({"code": code, "texture": t["rel"]})
                elif t["width"] and max(t["width"], t["height"]) > 2048:
                    big_tex.append({"code": code, "texture": t["rel"], "px": t["width"]})
            if it.get("kind") == "model":
                dims = it.get("dimensions_m")
                # A flat prop (zero thickness) is legitimate; only a missing or
                # all-zero box is a defect.
                if not dims or not any(float(dims.get(k, 0)) > 0 for k in ("width", "height", "depth")):
                    null_dims.append(code)
                if (it.get("tri_count") or 0) > 50000 and len(it.get("lods") or []) <= 1:
                    hi_single_lod.append({"code": code, "tris": it.get("tri_count")})
                if (it.get("n_tex") or 0) > 12:
                    many_tex.append({"code": code, "n_tex": it.get("n_tex")})
            if it.get("kind") == "rig" and not self.clips(it):
                stale_rigs.append(code)
        man = _load_json(MANIFEST) or {}
        newest = 0.0
        for it in items:
            d = self.asset_dir(it)
            if d.is_dir():
                for f in d.iterdir():
                    try:
                        newest = max(newest, f.stat().st_mtime)
                    except OSError:
                        pass
        return {
            "checked": len(items),
            "missing_preview": missing_preview[:50],
            "missing_preview_count": len(missing_preview),
            "missing_textures": missing_tex[:50],
            "missing_textures_count": len(missing_tex),
            "null_dimensions": null_dims[:50],
            "null_dimensions_count": len(null_dims),
            "highpoly_single_lod": sorted(hi_single_lod, key=lambda r: -r["tris"]),
            "many_textures": many_tex,
            "oversize_textures": big_tex[:50],
            "oversize_textures_count": len(big_tex),
            "rigs_without_clips": stale_rigs,
            "manifest_generated_at": man.get("generatedAt"),
            "newest_asset_edit": newest,
        }


LIB = Library()


# Words an agent is likely to use that the library's own names do not contain.
SYNONYMS: dict[str, tuple[str, ...]] = {
    "house": ("building", "hut", "home", "asianhouse", "architecture"),
    "home": ("building", "house", "architecture"),
    "building": ("house", "architecture", "asianhouse"),
    "roof": ("roof", "thatch", "tile", "eave", "temple"),
    "temple": ("temple", "asian", "shrine", "pillar", "gate"),
    "tree": ("vegetation", "tree", "birch", "oak", "pine", "spruce", "bamboo", "forest"),
    "forest": ("vegetation", "tree", "forest", "meadow", "beech"),
    "bush": ("vegetation", "shrub", "bush", "hedge"),
    "grass": ("vegetation", "grass", "meadow", "ground"),
    "plant": ("vegetation", "plant", "flower", "fern"),
    "rock": ("rock", "stone", "cliff", "boulder", "ore"),
    "cliff": ("rock", "cliff", "stone", "mountain"),
    "mountain": ("rock", "cliff", "terrain", "mountain"),
    "wall": ("wall", "fence", "barrier", "stonewall"),
    "fence": ("fence", "gate", "railing", "wall"),
    "gate": ("gate", "fence", "door", "torii"),
    "door": ("door", "gate", "window"),
    "bridge": ("bridge", "plank", "walkway"),
    "stairs": ("stairs", "staircase", "step"),
    "floor": ("floor", "ground", "path", "road"),
    "ground": ("ground", "terrain", "floor", "soil", "grass"),
    "water": ("water", "river", "lake", "pond"),
    "fire": ("fire", "torch", "campfire", "lantern"),
    "light": ("light", "lantern", "torch", "lamp"),
    "weapon": ("weapon", "sword", "axe", "spear", "katana", "dagger", "staff", "bow"),
    "sword": ("sword", "katana", "blade", "weapon"),
    "shield": ("shield", "armor", "weapon"),
    "barrel": ("barrel", "crate", "chest", "container"),
    "crate": ("crate", "box", "barrel", "container"),
    "furniture": ("chair", "table", "bench", "shelf", "bed"),
    "market": ("market", "stall", "cloth", "vendor"),
    "tavern": ("tavern", "inn", "market", "barrel", "table"),
    "village": ("building", "house", "market", "fence"),
    "ruin": ("ruin", "broken", "damaged", "old"),
    "modular": ("modular", "set", "kit"),
    "lowpoly": ("lowpoly", "low"),
    "realistic": ("realistic", "naturemanufacture"),
    "stylized": ("stylized", "polygon", "fantasy"),
    "character": ("rig", "humanoid", "character"),
    "animation": ("rig", "clips", "animset"),
    "walk": ("walk", "locomotion", "movement"),
    "run": ("run", "sprint", "locomotion"),
    "combat": ("fight", "kick", "fist", "h2h", "battle"),
    "material": ("material", "texture", "shader"),
}


