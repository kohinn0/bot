#!/usr/bin/env python3
"""
Generate a lightweight repo map for AI/code navigation.

Output: docs/repo-map.md
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path


EXCLUDE_DIRS = {
    ".git",
    "target",
    "node_modules",
    ".cursor",
    ".idea",
    "__pycache__",
}

TEXT_EXTS = {
    ".rs",
    ".py",
    ".sh",
    ".md",
    ".json",
    ".toml",
    ".yml",
    ".yaml",
    ".service",
}


def is_text_file(path: Path) -> bool:
    return path.suffix.lower() in TEXT_EXTS or path.name in {"Dockerfile", "Makefile"}


def should_skip(path: Path) -> bool:
    parts = set(path.parts)
    return bool(parts & EXCLUDE_DIRS)


def short_description(path: Path) -> str:
    name = path.name.lower()
    p = str(path).lower()
    if name == "main.rs":
        return "Application entrypoint and runtime orchestration."
    if "order_manager" in name:
        return "Order payload builder (ladder, TP/SL, close)."
    if "client" in name and "network" in p:
        return "REST client helpers and response guards."
    if "feed" in name:
        return "WebSocket feed handling and book/user events."
    if name == "setup.sh":
        return "Operational helper script for build/run/deploy."
    if name.endswith(".service"):
        return "Systemd service definition."
    if name.endswith(".json"):
        return "Configuration data."
    if name.endswith(".md"):
        return "Project documentation."
    return "Source/config file."


RUST_FN = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)")
RUST_STRUCT = re.compile(r"^\s*(?:pub\s+)?struct\s+([A-Za-z0-9_]+)")
RUST_ENUM = re.compile(r"^\s*(?:pub\s+)?enum\s+([A-Za-z0-9_]+)")
PY_DEF = re.compile(r"^\s*def\s+([a-zA-Z0-9_]+)\s*\(")
PY_CLASS = re.compile(r"^\s*class\s+([A-Za-z0-9_]+)")


def extract_symbols(path: Path) -> list[str]:
    symbols: list[str] = []
    try:
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except OSError:
        return symbols

    for line in lines:
        for rx in (RUST_STRUCT, RUST_ENUM, RUST_FN, PY_CLASS, PY_DEF):
            m = rx.search(line)
            if m:
                symbols.append(m.group(1))
                break

    deduped: list[str] = []
    seen = set()
    for s in symbols:
        if s in seen:
            continue
        seen.add(s)
        deduped.append(s)
        if len(deduped) >= 8:
            break
    return deduped


def collect_files(root: Path) -> list[Path]:
    out: list[Path] = []
    for p in sorted(root.rglob("*")):
        if not p.is_file():
            continue
        rel = p.relative_to(root)
        if should_skip(rel):
            continue
        if is_text_file(p):
            out.append(rel)
    return out


def render_markdown(root: Path, files: list[Path]) -> str:
    lines: list[str] = []
    lines.append("# Repo Map")
    lines.append("")
    lines.append("Auto-generated project map for fast code navigation and AI context.")
    lines.append("")
    lines.append("## Files")
    lines.append("")
    for rel in files:
        abs_path = root / rel
        desc = short_description(rel)
        symbols = extract_symbols(abs_path)
        lines.append(f"- `{rel}` — {desc}")
        if symbols:
            lines.append(f"  - symbols: `{', '.join(symbols)}`")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate repository map markdown.")
    parser.add_argument("--root", default=".", help="Repository root path.")
    parser.add_argument(
        "--output",
        default="docs/repo-map.md",
        help="Output markdown file path, relative to root.",
    )
    args = parser.parse_args()

    root = Path(args.root).resolve()
    output = (root / args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)

    files = collect_files(root)
    md = render_markdown(root, files)
    output.write_text(md, encoding="utf-8")
    print(f"Repo map written: {output}")
    print(f"Files indexed: {len(files)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
