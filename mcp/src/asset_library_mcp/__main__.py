"""Entry point for ``uv run asset-library-mcp`` (stdio transport)."""

from __future__ import annotations

from .server import mcp


def main() -> None:
    mcp.run()


if __name__ == "__main__":
    main()
