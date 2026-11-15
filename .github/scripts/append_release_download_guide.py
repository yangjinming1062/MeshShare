#!/usr/bin/env python3
"""Append the fixed, versioned download guide to generated release notes."""

from __future__ import annotations

import argparse
import re
from pathlib import Path
from urllib.parse import quote


TAG_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
PLACEHOLDER_RE = re.compile(r"\{\{([A-Z][A-Z0-9_]*)\}\}")
TEMPLATE_PATH = Path(__file__).resolve().parents[1] / "release-download-guide.md"

ASSET_NAMES = {
    "WINDOWS_NSIS_URL": "MeshShare_{version}_x64-setup.exe",
    "WINDOWS_MSI_URL": "MeshShare_{version}_x64_en-US.msi",
    "MACOS_ARM64_DMG_URL": "MeshShare_{version}_aarch64.dmg",
    "MACOS_X64_DMG_URL": "MeshShare_{version}_x64.dmg",
    "LINUX_APPIMAGE_URL": "MeshShare_{version}_amd64.AppImage",
    "LINUX_DEB_URL": "MeshShare_{version}_amd64.deb",
    "LINUX_RPM_URL": "MeshShare-{version}-1.x86_64.rpm",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", help="release tag such as v1.2.3")
    parser.add_argument("repository", help="GitHub repository such as owner/name")
    parser.add_argument("notes_file", type=Path, help="generated release notes file")
    parser.add_argument(
        "--template",
        type=Path,
        default=TEMPLATE_PATH,
        help="download guide template (default: .github/release-download-guide.md)",
    )
    return parser.parse_args()


def release_urls(tag: str, repository: str) -> dict[str, str]:
    match = TAG_RE.fullmatch(tag)
    if not match:
        raise ValueError(f"Expected a release tag such as v1.2.3, got {tag!r}")
    if not REPOSITORY_RE.fullmatch(repository):
        raise ValueError(f"Expected a GitHub repository such as owner/name, got {repository!r}")

    version = tag[1:]
    base_url = f"https://github.com/{repository}/releases/download/{quote(tag, safe='')}"
    return {
        placeholder: f"{base_url}/{quote(asset_name.format(version=version), safe='')}"
        for placeholder, asset_name in ASSET_NAMES.items()
    }


def render_template(template: str, urls: dict[str, str]) -> str:
    placeholders = set(PLACEHOLDER_RE.findall(template))
    expected = set(urls)
    missing = sorted(expected - placeholders)
    unknown = sorted(placeholders - expected)
    if missing or unknown:
        details = []
        if missing:
            details.append(f"missing placeholders: {', '.join(missing)}")
        if unknown:
            details.append(f"unknown placeholders: {', '.join(unknown)}")
        raise ValueError("Invalid download guide template (" + "; ".join(details) + ")")

    rendered = template
    for placeholder, url in urls.items():
        rendered = rendered.replace("{{" + placeholder + "}}", url)
    unresolved = sorted(set(PLACEHOLDER_RE.findall(rendered)))
    if unresolved:
        raise ValueError("Download guide contains unresolved placeholders: " + ", ".join(unresolved))
    return rendered.strip()


def append_guide(notes_file: Path, guide: str) -> None:
    notes = notes_file.read_text(encoding="utf-8").rstrip()
    combined = f"{notes}\n\n{guide}" if notes else guide
    with notes_file.open("w", encoding="utf-8", newline="\n") as output:
        output.write(combined + "\n")


def main() -> None:
    args = parse_args()
    try:
        guide = render_template(
            args.template.read_text(encoding="utf-8"),
            release_urls(args.tag, args.repository),
        )
        append_guide(args.notes_file, guide)
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from None


if __name__ == "__main__":
    main()
