"""Check release metadata validation without GitHub or a kernel build."""

import unittest

from release import release_notes


class ReleaseTests(unittest.TestCase):
    changelog = """## Не выпущено

future change

## 0.2.0 — 2026-10-13

### Добавлено

- Оболочка и тетрис.

## 0.1.0 — 2026-10-12

old change
"""

    def notes(self, tag="v0.2.0", version="0.2.0", changelog=None):
        return release_notes(tag, version, self.changelog if changelog is None else changelog,
                             "abcdef123456", "fedcba654321")

    def test_notes_identify_both_images_and_their_sources(self):
        notes = self.notes()
        for expected in ("OSel 0.2.0", "abcdef123456", "fedcba654321", "OSel.uefi.img",
                         "OSel.bios.img", "SHA256SUMS", "usb.html", "Оболочка и тетрис."):
            self.assertIn(expected, notes)
        self.assertNotIn("future change", notes)
        self.assertNotIn("old change", notes)

    def test_rejects_tags_outside_the_release_format(self):
        for tag in ("0.2.0", "v0.2", "v00.2.0", "v0.2.0-rc1", "v0.2.0\n"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                self.notes(tag=tag)

    def test_rejects_version_mismatch(self):
        with self.assertRaisesRegex(ValueError, "does not match"):
            self.notes(version="0.1.0")

    def test_rejects_missing_or_empty_release_changelog(self):
        for changelog in ("## Не выпущено\n", "## 0.2.0\n\nchanges\n",
                          "## 0.2.0 — 2026-10-13\n\n## 0.1.0 — 2026-10-12\nold\n"):
            with self.subTest(changelog=changelog), self.assertRaisesRegex(ValueError, "CHANGELOG"):
                self.notes(changelog=changelog)


if __name__ == "__main__":
    unittest.main()
