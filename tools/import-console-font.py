#!/usr/bin/env python3
"""Convert the pinned Terminus BDF sources to OSel's font tables on stdout."""

import pathlib
import re
import sys


def convert(path, name, height):
    source = pathlib.Path(path).read_text()
    width = height // 2
    assert f"FONTBOUNDINGBOX {width} {height} 0" in source
    glyphs = {}
    for block in source.split("STARTCHAR ")[1:]:
        code = int(re.search(r"^ENCODING (\d+)$", block, re.M)[1])
        bbox = re.search(r"^BBX (.+)$", block, re.M)[1].split()
        assert list(map(int, bbox[:3])) == [width, height, 0]
        rows = re.search(r"BITMAP\n(.*?)\nENDCHAR", block, re.S)[1].splitlines()
        assert len(rows) == height
        glyphs[code] = [int(row, 16) for row in rows]
    print("#[rustfmt::skip]")
    print(f"const {name}: [[u16; {height}]; 97] = [")
    for code in [*range(32, 127), 0x416, 0xFFFD]:
        values = ", ".join(f"0x{row:04x}" for row in glyphs[code])
        print(f"    [{values}], // U+{code:04X}")
    print("];")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: import-console-font.py ter-u16n.bdf ter-u32n.bdf")
    print("// SPDX-License-Identifier: OFL-1.1")
    print("// Copyright (C) 2020 Dimitar Toshkov Zhekov.")
    print("// OSel Console: subset of Terminus Font 4.49.1, with unmodified glyph bitmaps.")
    print("// Generated from ter-u16n.bdf and ter-u32n.bdf; see docs/ru/developer/architecture/drivers/console.md.")
    print()
    convert(sys.argv[1], "SMALL", 16)
    print()
    convert(sys.argv[2], "LARGE", 32)
