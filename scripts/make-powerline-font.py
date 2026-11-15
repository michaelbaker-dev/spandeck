#!/usr/bin/env python3
"""Make crates/web/assets/fonts/SpandeckPowerline-3.5.1.woff2.

The powerline separators (U+E0B0 to U+E0D7) of the Nerd Fonts symbols font,
fitted to a terminal cell the way the Nerd Fonts patcher fits them to the font
it patches. A browser cannot do that fitting: it draws a fallback font's glyph
at its own shape, and a separator has to be as tall as the row and no wider
than the cell or the prompt it belongs to has gaps in it.

The cell fitted to is 0.6 em wide and runs from 0.25 em under the baseline to
0.95 em over it, which is what most monospace fonts have (DejaVu Sans Mono,
Menlo, SF Mono, Fira Code, Cascadia Code, Consolas are within a few
hundredths). Nothing else in the glyphs is changed.

Run once, by hand, when the symbols font is updated:

    python3 scripts/make-powerline-font.py NerdFontsSymbolsOnly.zip

where the zip is the "NerdFontsSymbolsOnly" archive of the same Nerd Fonts
release as the bundled SymbolsNerdFontMono file. Needs fonttools and brotli.
"""
import io
import os
import sys
import zipfile

from fontTools import subset
from fontTools.pens.recordingPen import DecomposingRecordingPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont

SOURCE = "SymbolsNerdFont-Regular.ttf"  # the variant whose glyphs start at the cell's left edge
FIRST, LAST = 0xE0B0, 0xE0D7
CELL, UNDER, OVER = 0.6, 0.25, 0.95  # of an em
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "web", "assets", "fonts", "SpandeckPowerline-3.5.1.woff2")


def main(archive):
    font = TTFont(io.BytesIO(zipfile.ZipFile(archive).read(SOURCE)))
    options = subset.Options()
    options.layout_features = []
    options.name_IDs = [1, 2, 4, 6]
    options.notdef_outline = False
    picker = subset.Subsetter(options)
    picker.populate(unicodes=range(FIRST, LAST + 1))
    picker.subset(font)

    em = font["head"].unitsPerEm
    cmap = font.getBestCmap()
    glyphs, widths = font.getGlyphSet(), font["hmtx"].metrics
    # The plain separator (U+E0B0) is the source font's idea of one cell wide;
    # its line runs from descent to ascent.
    sx = CELL * em / widths[cmap[FIRST]][0]
    low, high = font["hhea"].descent, font["hhea"].ascent
    sy = (UNDER + OVER) * em / (high - low)
    dy = -UNDER * em - low * sy
    made = {}
    for name in set(cmap.values()):
        drawn = DecomposingRecordingPen(glyphs)
        glyphs[name].draw(drawn)
        pen = TTGlyphPen(None)
        drawn.replay(TransformPen(pen, (sx, 0, 0, sy, 0, dy)))
        made[name] = pen.glyph()
    for name, glyph in made.items():
        font["glyf"][name] = glyph
        glyph.recalcBounds(font["glyf"])
        widths[name] = (round(widths[name][0] * sx), glyph.xMin if glyph.numberOfContours else 0)

    font["hhea"].ascent = font["OS/2"].sTypoAscender = font["OS/2"].usWinAscent = round(OVER * em)
    font["hhea"].descent = font["OS/2"].sTypoDescender = -round(UNDER * em)
    font["OS/2"].usWinDescent = round(UNDER * em)
    font["hhea"].lineGap = font["OS/2"].sTypoLineGap = 0
    for record in font["name"].names:
        text = {1: "Spandeck Powerline", 2: "Regular", 4: "Spandeck Powerline", 6: "SpandeckPowerline"}.get(record.nameID)
        if text:
            record.string = text
    font.flavor = "woff2"
    font.save(OUT)
    print(f"{os.path.normpath(OUT)}: {len(cmap)} glyphs, {os.path.getsize(OUT)} bytes")


if __name__ == "__main__":
    main(sys.argv[1])
