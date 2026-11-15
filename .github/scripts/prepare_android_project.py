#!/usr/bin/env python3
"""Apply MeshShare's Android manifest and LAN transport policy after Tauri init."""

import re
from pathlib import Path


def replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old not in text:
        if new in text:
            return
        raise SystemExit(f"Expected Android template text was not found in {path}: {old}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


def ensure_root_dir_rel(path: Path) -> None:
    """Normalize the Rust project path across Tauri Android templates."""

    text = path.read_text(encoding="utf-8")
    target = '    rootDirRel = "../../../"'
    if target in text:
        return
    for old in (
        '    rootDirRel = "../../../../frontend"',
        '    rootDirRel = "../../../../"',
    ):
        if old in text:
            path.write_text(text.replace(old, target, 1), encoding="utf-8")
            return
    raise SystemExit(f"Expected Android Rust rootDirRel setting was not found in {path}")


def configure_signing(gradle: Path) -> None:
    """Wire an optional app-local signing properties file into release builds."""

    properties = gradle.parent / "android-signing.properties"
    if not properties.is_file():
        return

    text = gradle.read_text(encoding="utf-8")
    if "val androidSigningProperties" not in text:
        signing_properties = '''val androidSigningProperties = Properties().apply {
    val propFile = file("android-signing.properties")
    propFile.inputStream().use { load(it) }
}

'''
        marker = "val tauriProperties = Properties().apply {"
        if marker not in text:
            raise SystemExit(f"Expected Tauri properties marker was not found in {gradle}")
        text = text.replace(marker, signing_properties + marker, 1)

    if "signingConfigs {" not in text:
        marker = "    buildTypes {\n"
        signing_config = '''    signingConfigs {
        create("release") {
            fun required(name: String): String =
                androidSigningProperties.getProperty(name)
                    ?: error("Missing Android signing property: $name")
            storeFile = file(required("storeFile"))
            storePassword = required("storePassword")
            keyAlias = required("keyAlias")
            keyPassword = required("keyPassword")
        }
    }
'''
        if marker not in text:
            raise SystemExit(f"Expected Android build type marker was not found in {gradle}")
        text = text.replace(marker, signing_config + marker, 1)

    if 'signingConfig = signingConfigs.getByName("release")' not in text:
        marker = '        getByName("release") {\n'
        if marker not in text:
            raise SystemExit(f"Expected Android release build type was not found in {gradle}")
        text = text.replace(
            marker,
            marker + '            signingConfig = signingConfigs.getByName("release")\n',
            1,
        )

    gradle.write_text(text, encoding="utf-8")


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    android = root / "backend" / "gen" / "android"
    manifest = android / "app" / "src" / "main" / "AndroidManifest.xml"
    gradle = android / "app" / "build.gradle.kts"
    if not manifest.is_file() or not gradle.is_file():
        raise SystemExit("Run `tauri android init` before preparing the Android project")

    if "android.permission.ACCESS_NETWORK_STATE" not in manifest.read_text(encoding="utf-8"):
        replace_once(
            manifest,
            '    <uses-permission android:name="android.permission.INTERNET" />\n',
            '    <uses-permission android:name="android.permission.INTERNET" />\n'
            '    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />\n'
            '    <uses-permission android:name="android.permission.ACCESS_WIFI_STATE" />\n'
            '    <uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />\n',
        )
    replace_once(
        gradle,
        '        manifestPlaceholders["usesCleartextTraffic"] = "false"',
        '        // MeshShare deliberately trusts the configured LAN boundary.\n'
        '        manifestPlaceholders["usesCleartextTraffic"] = "true"',
    )
    ensure_root_dir_rel(gradle)
    gradle_text = gradle.read_text(encoding="utf-8")
    gradle_text = re.sub(r"    compileSdk = \d+", "    compileSdk = 36", gradle_text, count=1)
    gradle_text = re.sub(r"        targetSdk = \d+", "        targetSdk = 35", gradle_text, count=1)
    gradle.write_text(gradle_text, encoding="utf-8")
    configure_signing(gradle)

    manifest_lines = manifest.read_text(encoding="utf-8").splitlines(keepends=True)
    seen_permissions: set[str] = set()
    deduplicated: list[str] = []
    for line in manifest_lines:
        match = re.search(r'uses-permission android:name="([^"]+)"', line)
        if match:
            permission = match.group(1)
            if permission in seen_permissions:
                continue
            seen_permissions.add(permission)
        deduplicated.append(line)
    manifest.write_text("".join(deduplicated), encoding="utf-8")


if __name__ == "__main__":
    main()
