#!/usr/bin/env python3
"""Check the exact, unmodified Wi-Fi firmware bytes and required redistribution notices."""
import hashlib
import json
from pathlib import Path

def main():
    root = Path(__file__).resolve().parents[1] / "firmware"
    manifest = json.loads((root / "manifest.json").read_text())
    for key in ("license", "notice"):
        if not (root / manifest[key]).read_bytes():
            raise SystemExit(f"firmware {key} is empty")
    for item in manifest["files"]:
        data = (root / item["path"]).read_bytes()
        if len(data) != item["bytes"] or hashlib.sha256(data).hexdigest() != item["sha256"]:
            raise SystemExit(f"firmware differs from the recorded upstream bytes: {item['path']}")
    print("Firmware OK: exact bytes, license and notice")

if __name__ == "__main__":
    main()
