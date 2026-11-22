#!/usr/bin/env python3
"""将 MeshShare 的插件、资源和构建设置写入 Tauri 生成的 Android 工程。"""

import re
import shutil
import xml.etree.ElementTree as ET
from pathlib import Path


ANDROID = "{http://schemas.android.com/apk/res/android}"
PERMISSIONS = (
    "ACCESS_NETWORK_STATE",
    "ACCESS_WIFI_STATE",
    "CHANGE_WIFI_MULTICAST_STATE",
    "MANAGE_EXTERNAL_STORAGE",
    "REQUEST_INSTALL_PACKAGES",
    "UPDATE_PACKAGES_WITHOUT_USER_ACTION",
)
RECEIVER = "com.meshshare.android.update.UpdateResultReceiver"


def configure_manifest(path: Path) -> None:
    ET.register_namespace("android", ANDROID[1:-1])
    parser = ET.XMLParser(target=ET.TreeBuilder(insert_comments=True))
    tree = ET.parse(path, parser)
    root = tree.getroot()
    name = ANDROID + "name"
    application = root.find("application")
    if application is None:
        raise ValueError(f"Missing Android application in {path}")

    existing = {item.get(name) for item in root.findall("uses-permission")}
    for permission in PERMISSIONS:
        full_name = "android.permission." + permission
        if full_name not in existing:
            root.insert(0, ET.Element("uses-permission", {name: full_name}))
    application.set(ANDROID + "requestLegacyExternalStorage", "true")
    if not any(item.get(name) == RECEIVER for item in application.findall("receiver")):
        ET.SubElement(application, "receiver", {name: RECEIVER, ANDROID + "exported": "false"})
    ET.indent(tree)
    tree.write(path, encoding="utf-8", xml_declaration=True)


def configure_gradle(path: Path) -> None:
    text = path.read_text(encoding="utf-8")

    for key, value in (("rootDirRel", '"../../../"'), ("compileSdk", "36"), ("targetSdk", "35")):
        text, count = re.subn(rf"(?m)^( *{key} = ).+$", rf"\g<1>{value}", text)
        if count != 1:
            raise ValueError(f"Expected one {key} setting in {path}")

    # defaultConfig 同时作用于发布和调试构建，debug 还可以单独覆写。
    text, count = re.subn(
        r'(defaultConfig\s*\{[^}]*?manifestPlaceholders\["usesCleartextTraffic"\] = )"(?:true|false)"',
        r'\1"true"', text, count=1,
    )
    if not count:
        raise ValueError(f"Missing usesCleartextTraffic in {path}")

    if (path.parent / "android-signing.properties").is_file():
        config = '''    signingConfigs {
        create("release") {
            val props = Properties()
            file("android-signing.properties").inputStream().use { props.load(it) }
            fun required(key: String) = props.getProperty(key) ?: error("Missing signing property: $key")
            storeFile = file(required("storeFile"))
            storePassword = required("storePassword")
            keyAlias = required("keyAlias")
            keyPassword = required("keyPassword")
        }
    }
'''
        marker = '        getByName("release") {\n'
        signing = '            signingConfig = signingConfigs.getByName("release")\n'
        if signing not in text:
            if "    buildTypes {\n" not in text or marker not in text:
                raise ValueError(f"Missing Android release build type in {path}")
            text = text.replace("    buildTypes {\n", config + "    buildTypes {\n", 1)
            text = text.replace(marker, marker + signing, 1)
    path.write_text(text, encoding="utf-8")


def main() -> None:
    backend = Path(__file__).resolve().parents[2] / "backend"
    app = backend / "gen/android/app"
    src = app / "src/main"
    if not (src / "AndroidManifest.xml").is_file():
        raise SystemExit("Run `tauri android init` before preparing the Android project")
    configure_manifest(src / "AndroidManifest.xml")
    configure_gradle(app / "build.gradle.kts")
    shutil.copytree(backend / "icons/mobile/android", src / "res", dirs_exist_ok=True)
    for plugin in ("Network", "Storage", "Update"):
        package = src / "java/com/meshshare/android" / plugin.lower()
        package.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(backend / "android" / f"{plugin}Plugin.kt", package / f"{plugin}Plugin.kt")


if __name__ == "__main__":
    main()
