# Asset Library MCP

Agent-facing MCP server for the same local 3D asset library the desktop app browses.
It runs over **stdio** and exposes 34 tools for search, inspection, tagging,
maintenance, LODs/textures, the shared cart, and Unity import.

The server owns no data of its own:

| Shared with the desktop app | Path |
|---|---|
| Library + manifest | `D:\3D Resource\_Catalog\manifest.json` |
| Per-asset sidecars | `<library>\Models\|Materials\|Rig\...\<slug>.json` |
| Cart | `%APPDATA%\asset-library-desktop\store.json` |
| Authoring tools | `<library>\_Catalog\tools\*.py` |

## Setup

```powershell
cd D:\Dev\asset-library-desktop\mcp
uv sync
uv run asset-library-mcp
```

`uv run` is enough; the checked-in `uv.lock` pins the environment.

## Register with opencode

```jsonc
"asset-library": {
  "type": "local",
  "command": ["uv", "run", "--directory", "D:\\Dev\\asset-library-desktop\\mcp", "asset-library-mcp"],
  "enabled": true
}
```

## Environment

- `ASSET_LIBRARY_ROOT` - library root (default `D:\3D Resource`).
- The DeepSeek key/model used by the tagging tools comes from the desktop app's
  `store.json` (Settings -> DeepSeek), so it never lives in this repo.

## Tool groups

- **Read** - `search_assets`, `find_by_description`, `find_similar`, `get_asset`,
  `get_preview`, `get_paths`, `list_textures`, `list_clips`, `library_stats`,
  `library_facets`, `library_taxonomy`.
- **Cart** - `cart_list`, `cart_add`, `cart_remove`, `cart_clear`, `cart_export`.
- **Authoring** - `asset_create`, `asset_update`, `asset_next_code`,
  `asset_auto_tag`, `asset_auto_tag_batch`, `asset_revisions`, `asset_restore`.
- **Maintenance** - `library_qa`, `texture_audit`, `texture_downscale`,
  `preview_regenerate`, `lod_generate`, `find_texture_usage`.
- **Unity** - `unity_status`, `unity_import_batch`, `unity_import_rig`,
  `unity_sync_previews`.
