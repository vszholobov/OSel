#!/usr/bin/env python3
"""Tests for bilingual output checks and legacy redirects, without building a site."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


spec = importlib.util.spec_from_file_location("docs_site", Path(__file__).with_name("docs-site.py"))
docs_site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docs_site)


class SiteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.docs = root / "docs"
        self.site = root / "site"
        for lang in ("ru", "en"):
            for page in ("index", "user/index", "developer/index", "user/usb"):
                source = self.docs / lang / f"{page}.md"
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text("# OSel\n")
                output = self.site / lang / f"{page}.html"
                output.parent.mkdir(parents=True, exist_ok=True)
                output.write_text("<html>OSel</html>")
        self.redirects = {"index.html": "ru/index.html", "start/usb.html": "ru/user/usb.html"}
        self.write_redirects()

    def write_redirects(self):
        (self.docs / "redirects.json").write_text(json.dumps(self.redirects))

    def test_old_urls_resolve_relative_to_their_own_directory(self):
        docs_site.prepare_site(self.site, self.docs)
        self.assertIn('url=ru/index.html', (self.site / "index.html").read_text())
        self.assertIn('url=../ru/user/usb.html', (self.site / "start/usb.html").read_text())
        self.assertEqual((self.site / "en/user/usb.html").read_text(), "<html>OSel</html>")

    def test_missing_english_output_fails_before_writing_redirects(self):
        (self.site / "en/user/usb.html").unlink()
        with self.assertRaisesRegex(RuntimeError, "Missing rendered page"):
            docs_site.prepare_site(self.site, self.docs)
        self.assertFalse((self.site / "index.html").exists())

    def test_missing_redirect_destination_fails(self):
        self.redirects = {"old.html": "ru/user/missing.html"}
        self.write_redirects()
        with self.assertRaisesRegex(RuntimeError, "points to missing page"):
            docs_site.prepare_site(self.site, self.docs)


if __name__ == "__main__":
    unittest.main()
