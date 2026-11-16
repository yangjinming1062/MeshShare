#!/usr/bin/env python3
"""Apply MeshShare's Android sources, manifest and transport policy after Tauri init."""

import re
import shutil
from pathlib import Path


def replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if new in text:
        return
    if old not in text:
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


def install_android_icons(root: Path, android: Path) -> None:
    """把仓库内的品牌启动图标写入生成的 Android 工程。"""

    source = root / "backend" / "icons" / "mobile" / "android"
    res = android / "app" / "src" / "main" / "res"
    if not source.is_dir():
        raise SystemExit(f"Missing Android launcher icon assets: {source}")
    if not (res / "mipmap-mdpi").is_dir():
        raise SystemExit(
            "Expected mipmap resources in the generated Android project: "
            f"{res / 'mipmap-mdpi'}"
        )
    copied = 0
    for asset in sorted(source.rglob("*")):
        if not asset.is_file():
            continue
        target = res / asset.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(asset.read_bytes())
        copied += 1
    print(f"Installed {copied} Android launcher icon resources")


def install_android_updater(root: Path, manifest: Path) -> None:
    source = root / "backend" / "android"
    target = manifest.parent / "java" / "com" / "meshshare" / "android" / "update"
    target.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source / "UpdatePlugin.kt", target / "UpdatePlugin.kt")
    replace_once(
        manifest,
        '    <uses-permission android:name="android.permission.INTERNET" />\n',
        '    <uses-permission android:name="android.permission.REQUEST_INSTALL_PACKAGES" />\n'
        '    <uses-permission android:name="android.permission.UPDATE_PACKAGES_WITHOUT_USER_ACTION" />\n'
        '    <uses-permission android:name="android.permission.INTERNET" />\n',
    )
    replace_once(
        manifest,
        '    </application>',
        '        <receiver\n'
        '            android:name="com.meshshare.android.update.UpdateResultReceiver"\n'
        '            android:exported="false" />\n'
        '    </application>',
    )


def install_android_network(root: Path, manifest: Path) -> None:
    """Install the native LAN/VPN network adapter used by the Android node."""

    source = root / "backend" / "android" / "NetworkPlugin.kt"
    target = (
        manifest.parent
        / "java"
        / "com"
        / "meshshare"
        / "android"
        / "network"
        / "NetworkPlugin.kt"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)


def install_android_storage(root: Path, manifest: Path) -> None:
    """Install the storage plugin that writes received files to public Downloads."""

    source = root / "backend" / "android" / "StoragePlugin.kt"
    target = (
        manifest.parent
        / "java"
        / "com"
        / "meshshare"
        / "android"
        / "storage"
        / "StoragePlugin.kt"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    text = manifest.read_text(encoding="utf-8")
    if "android.permission.MANAGE_EXTERNAL_STORAGE" not in text:
        text = text.replace(
            '    <uses-permission android:name="android.permission.INTERNET" />\n',
            '    <uses-permission android:name="android.permission.INTERNET" />\n'
            '    <uses-permission android:name="android.permission.MANAGE_EXTERNAL_STORAGE" />\n',
            1,
        )
    if "requestLegacyExternalStorage" not in text:
        text = text.replace(
            '        android:label="@string/app_name"\n',
            '        android:label="@string/app_name"\n'
            '        android:requestLegacyExternalStorage="true"\n',
            1,
        )
    if "requestLegacyExternalStorage" not in text:
        text = re.sub(
            r"(<application\b)",
            r'\1\n        android:requestLegacyExternalStorage="true"',
            text,
            count=1,
        )
    manifest.write_text(text, encoding="utf-8")


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
    install_android_network(root, manifest)
    install_android_icons(root, android)
    install_android_updater(root, manifest)
    install_android_storage(root, manifest)

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
