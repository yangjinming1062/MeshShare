#!/usr/bin/env python3
"""Build the signed Tauri updater manifest from one GitHub release's assets."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path


VERSION_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
APK_PACKAGE_RE = re.compile(
    r"^package: name='([^']+)' versionCode='([0-9]+)' versionName='([^']*)'",
    re.MULTILINE,
)


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


def android_entry(root: Path, version: str) -> dict[str, int | str]:
    artifact = "MeshShare-android-arm64"
    apk = find_asset(root, artifact, f"MeshShare_{version}_android_arm64.apk")
    badging = find_asset(root, artifact, "android-apk.badging").read_text(encoding="utf-8")
    package = APK_PACKAGE_RE.search(badging)
    if not package or package[1] != "com.meshshare.android" or package[3] != version:
        raise SystemExit(f"Android APK package or version does not match v{version}")

    major, minor, patch = map(int, version.split("."))
    code = int(package[2])
    if minor >= 1000 or patch >= 1000 or not 0 < code <= 2_100_000_000 or (
        code != major * 1_000_000 + minor * 1_000 + patch
    ):
        raise SystemExit(f"Invalid Android versionCode {code} for v{version}")
    if "native-code: 'arm64-v8a'" not in badging.splitlines():
        raise SystemExit("Expected an arm64 Android APK")

    digest = hashlib.sha256()
    with apk.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return {"size": apk.stat().st_size, "sha256": digest.hexdigest(), "version_code": code}


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
        "android": android_entry(args.assets_root, args.version),
    }


def main() -> None:
    args = parse_args()
    manifest = build_manifest(args)
    args.output.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
