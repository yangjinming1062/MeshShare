#!/usr/bin/env python3
"""Validate the updater private-key secret without printing its contents."""

from __future__ import annotations

import base64
import binascii
import os
import sys


def main() -> int:
    encoded = os.environ.get("TAURI_SIGNING_PRIVATE_KEY", "")
    if not encoded:
        print(
            "::error::TAURI_SIGNING_PRIVATE_KEY is not configured; add the complete key-file value to Actions Secrets"
        )
        return 1

    try:
        decoded = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as error:
        print(
            "::error::TAURI_SIGNING_PRIVATE_KEY must be the unmodified single-line base64 value from tauri signer generate"
        )
        print(f"::error::updater private-key base64 validation failed: {error}")
        return 1

    if not decoded.startswith(b"untrusted comment: rsign encrypted secret key\n"):
        print(
            "::error::TAURI_SIGNING_PRIVATE_KEY decoded, but is not a Tauri/minisign private-key file"
        )
        return 1

    print("Updater signing key format is valid.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
