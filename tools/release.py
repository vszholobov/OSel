#!/usr/bin/env python3
"""Validate a release tag and prepare English release notes from the changelog."""

import argparse
import pathlib
import re
import subprocess
import tomllib


def release_notes(tag, version, changelog, commit, tetris_commit):
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)", tag):
        raise ValueError("release tag must have the form vX.Y")
    public_version = tag[1:]
    if version != f"{public_version}.0":
        raise ValueError(f"tag {tag} does not match Cargo.toml version {version}")
    section = re.search(
        rf"^## {re.escape(public_version)} — \d{{4}}-\d{{2}}-\d{{2}}\n(.*?)(?=^## |\Z)",
        changelog, re.MULTILINE | re.DOTALL,
    )
    if not section or not section[1].strip():
        raise ValueError(f"CHANGELOG.md must contain a nonempty dated section for {public_version}")
    return f"""OSel {public_version} (build version `{version}`), commit `{commit}`.

{section[1].strip()}

### Downloads

- `OSel.uefi.img` — UEFI disk image.
- `OSel.bios.img` — legacy BIOS/CSM disk image.
- `SHA256SUMS` — SHA-256 checksums for both images.

Both images passed boot, shell, keyboard and Tetris checks in QEMU.
[Verify and write an image to USB](https://vszholobov.github.io/OSel/en/user/usb.html).

Tetris source: `{tetris_commit}`.
"""



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]

    def git(*arguments):
        return subprocess.check_output(["git", *arguments], cwd=root, text=True).strip()

    commit = git("rev-parse", "HEAD")
    if git("rev-parse", f"refs/tags/{args.tag}^{{commit}}") != commit:
        parser.error("release tag does not point to the checked-out commit")
    try:
        notes = release_notes(
            args.tag, version, (root / "CHANGELOG.md").read_text(),
            commit, git("rev-parse", "HEAD:apps/tetris"),
        )
    except ValueError as error:
        parser.error(str(error))
    args.output.write_text(notes)


if __name__ == "__main__":
    main()
