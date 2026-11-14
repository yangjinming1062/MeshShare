#!/usr/bin/env python3
"""Build the signed Tauri updater manifest from one GitHub release's assets."""

from __future__ import annotations

import argparse
import json
import re
from datetime import datetime, timezone
from pathlib import Path


VERSION_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets-root", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--notes-file", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def find_asset(root: Path, artifact_dir: str, suffix: str) -> Path:
    matches = sorted(
        path
        for path in (root / artifact_dir).rglob("*")
        if path.is_file() and path.name.endswith(suffix)
    )
    if len(matches) != 1 or matches[0].stat().st_size == 0:
        names = ", ".join(str(path) for path in matches) or "none"
        raise SystemExit(
            f"expected exactly one nonempty asset ending in {suffix!r} under "
            f"{artifact_dir!r}, found {names}"
        )
    return matches[0]


def platform_entry(root: Path, artifact_dir: str, suffix: str, tag: str) -> dict[str, str]:
    asset = find_asset(root, artifact_dir, suffix)
    signature = find_asset(root, artifact_dir, f"{asset.name}.sig")
    signature_text = signature.read_text(encoding="utf-8").strip()
    if not signature_text:
        raise SystemExit(f"signature is empty: {signature}")
    return {
        "url": f"https://github.com/yangjinming1062/MeshShare/releases/download/{tag}/{asset.name}",
        "signature": signature_text,
    }


def build_manifest(args: argparse.Namespace) -> dict[str, object]:
    if not VERSION_RE.fullmatch(args.version):
        raise SystemExit(f"invalid release version: {args.version}")
    if args.tag != f"v{args.version}":
        raise SystemExit(f"tag {args.tag!r} does not match version {args.version!r}")
    notes = args.notes_file.read_text(encoding="utf-8")

    windows_nsis = platform_entry(
        args.assets_root, "MeshShare-windows-x64", "-setup.exe", args.tag
    )
    windows_msi = platform_entry(
        args.assets_root, "MeshShare-windows-x64", "_en-US.msi", args.tag
    )
    mac_x64 = platform_entry(
        args.assets_root, "MeshShare-macos-x64", ".app.tar.gz", args.tag
    )
    mac_arm64 = platform_entry(
        args.assets_root, "MeshShare-macos-arm64", ".app.tar.gz", args.tag
    )
    linux_appimage = platform_entry(
        args.assets_root, "MeshShare-linux-x64", ".AppImage", args.tag
    )
    linux_deb = platform_entry(
        args.assets_root, "MeshShare-linux-x64", ".deb", args.tag
    )
    linux_rpm = platform_entry(
        args.assets_root, "MeshShare-linux-x64", ".rpm", args.tag
    )

    platforms = {
        "windows-x86_64-nsis": windows_nsis,
        "windows-x86_64-msi": windows_msi,
        "darwin-x86_64-app": mac_x64,
        "darwin-aarch64-app": mac_arm64,
        "linux-x86_64-appimage": linux_appimage,
        "linux-x86_64-deb": linux_deb,
        "linux-x86_64-rpm": linux_rpm,
    }
    return {
        "version": args.version,
        "notes": notes,
        "pub_date": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platforms": platforms,
    }


def main() -> None:
    args = parse_args()
    manifest = build_manifest(args)
    args.output.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
