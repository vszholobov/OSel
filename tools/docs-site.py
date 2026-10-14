#!/usr/bin/env python3
"""Check the bilingual CI output and preserve previous public page addresses."""

import argparse
import html
import json
import posixpath
from pathlib import Path


def prepare_site(site: Path, docs: Path) -> None:
    for lang in ("ru", "en"):
        for page in (docs / lang).rglob("*.md"):
            target = site / page.relative_to(docs).with_suffix(".html")
            if not target.is_file():
                raise RuntimeError(f"Missing rendered page: {target}")
        for audience in ("user", "developer"):
            index = site / lang / audience / "index.html"
            if not index.is_file():
                raise RuntimeError(f"Missing audience entry: {index}")

    redirects = json.loads((docs / "redirects.json").read_text())
    for old, new in redirects.items():
        target = site / new
        if not target.is_file():
            raise RuntimeError(f"Redirect {old} points to missing page {new}")
        relative = posixpath.relpath(new, posixpath.dirname(old) or ".")
        url = html.escape(relative, quote=True)
        output = site / old
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            '<!doctype html><html lang="ru"><meta charset="utf-8">'
            f'<meta http-equiv="refresh" content="0; url={url}">'
            f'<link rel="canonical" href="{url}">'
            f'<title>OSel</title><a href="{url}">OSel</a></html>\n'
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--site", required=True, type=Path)
    parser.add_argument("--docs", default=Path("docs"), type=Path)
    args = parser.parse_args()
    prepare_site(args.site, args.docs)
