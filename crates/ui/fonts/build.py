# Builds "Osmosis Sans", the app's one type family. Entry point: `just font-subset`
# (runs inside a fonttools nix-shell; the dependencies are not on the system).
#
#   desktop/osmosis-sans-{400,600,700}.ttf  Figtree (Latin, digits) merged with Noto Sans CJK SC
#                                           cut to the standard charsets below.
#   android/osmosis-sans-{400,600,700}.ttf  Figtree only; Android's system CJK fallback is
#                                           Noto Sans CJK, the same design.
#
# Why one merged family: Slint walks the system fallback chain for any glyph the family lacks,
# and on this desktop the first CJK font in that chain is Maple Mono (the old "monospace look").
# Decisions and measurements: docs/design.md "字体".
#
# - tnum is frozen into the Latin part (Slint has no font-feature-settings), so every digit is
#   tabular and durations line up without a mono face.
# - The CJK part is converted from CFF to TrueType outlines: fontTools.merge needs one outline
#   format, and Figtree is TrueType.
# - Vertical metrics are Figtree's (hhea 950/-250), so line heights match the Latin design.
#
# Inputs: src/figtree-*.ttf (static instances of Figtree, OFL) and the Noto Sans CJK variable
# collection that fc-match finds (nixpkgs noto-fonts-cjk-sans). Pass --check to report which
# titles and artists in the dev cache (~/.local/state/osmosis-dev) fall outside the desktop subset.

import argparse
import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

from fontTools import subset
from fontTools.merge import Merger
from fontTools.pens.cu2quPen import Cu2QuPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTCollection, TTFont, newTable
from fontTools.varLib.instancer import instantiateVariableFont

HERE = Path(__file__).parent
WEIGHTS = {400: 'Regular', 600: 'SemiBold', 700: 'Bold'}
FAMILY = 'Osmosis Sans'


def codec_chars(encoding: str, lead: range, trail) -> set[str]:
    chars = set()
    for a in lead:
        for b in trail:
            try:
                chars.update(bytes([a, b]).decode(encoding))
            except UnicodeDecodeError:
                pass
    return chars


def desktop_charset() -> set[int]:
    """Standard charsets, not today's library: song titles are dynamic.

    GB2312 (simplified), JIS X 0208 (kana, level 1+2 kanji), Big5 level 1 (traditional
    titles), KS X 1001 Hangul (2350 syllables), plus Latin-1/Extended, punctuation, CJK
    symbols and full-width forms.
    """
    chars = codec_chars('gb2312', range(0xA1, 0xF8), range(0xA1, 0xFF))
    chars |= codec_chars('euc_jp', range(0xA1, 0xF5), range(0xA1, 0xFF))
    chars |= codec_chars('big5', range(0xA4, 0xC7), [*range(0x40, 0x7F), *range(0xA1, 0xFF)])
    chars |= codec_chars('euc_kr', range(0xB0, 0xC9), range(0xA1, 0xFF))
    points = {ord(c) for c in chars}
    for lo, hi in ((0x20, 0x7F), (0xA0, 0x250), (0x2000, 0x2070), (0x3000, 0x3100), (0xFF00, 0xFFF0)):
        points.update(range(lo, hi))
    return points


def noto_sc() -> TTFont:
    """The variable SC face (CFF2 can't be subset before instancing)."""
    path = subprocess.run(['fc-match', '-f', '%{file}', 'Noto Sans CJK SC'],
                          check=True, capture_output=True, text=True).stdout
    for font in TTCollection(path).fonts:
        if font['name'].getDebugName(1).startswith('Noto Sans CJK SC'):
            if 'fvar' not in font:
                sys.exit(f'{path}: need the variable Noto Sans CJK collection')
            return font
    sys.exit(f'{path} has no Noto Sans CJK SC')


def freeze_tnum(font: TTFont) -> None:
    """Point the digits' cmap entries at their tabular glyphs."""
    gsub = font['GSUB'].table
    lookups = {i for rec in gsub.FeatureList.FeatureRecord if rec.FeatureTag == 'tnum'
               for i in rec.Feature.LookupListIndex}
    mapping = {}
    for i in lookups:
        for sub in gsub.LookupList.Lookup[i].SubTable:
            mapping.update(getattr(sub, 'mapping', {}))
    if not mapping:
        sys.exit('Figtree has no tnum substitutions')
    for table in font['cmap'].tables:
        for cp, glyph in list(table.cmap.items()):
            table.cmap[cp] = mapping.get(glyph, glyph)


def to_truetype(font: TTFont) -> None:
    """CFF to glyf with cu2qu (fontTools' Snippets/otf2ttf.py, inlined)."""
    order = font.getGlyphOrder()
    glyph_set = font.getGlyphSet()
    glyphs = {}
    for name in order:
        pen = TTGlyphPen(glyph_set)
        glyph_set[name].draw(Cu2QuPen(pen, 1.0, reverse_direction=True))
        glyphs[name] = pen.glyph()
    font['loca'] = newTable('loca')
    font['glyf'] = glyf = newTable('glyf')
    glyf.glyphOrder = order
    glyf.glyphs = glyphs
    for tag in ('CFF ', 'CFF2', 'VORG'):
        if tag in font:
            del font[tag]
    glyf.compile(font)
    for name in order:
        glyph = glyf[name]
        width, _ = font['hmtx'][name]
        font['hmtx'][name] = (width, glyph.xMin if hasattr(glyph, 'xMin') else 0)
    maxp = font['maxp'] = newTable('maxp')
    maxp.tableVersion = 0x00010000
    for field in ('maxZones', 'maxTwilightPoints', 'maxStorage', 'maxFunctionDefs',
                  'maxInstructionDefs', 'maxStackElements', 'maxSizeOfInstructions',
                  'maxComponentElements'):
        setattr(maxp, field, 0)
    maxp.maxZones = 1
    maxp.compile(font)
    post = font['post']
    post.formatType = 3.0
    font.sfntVersion = '\x00\x01\x00\x00'


def rename(font: TTFont, weight: int, latin: TTFont) -> None:
    style = WEIGHTS[weight]
    name = font['name']
    keep = {r.nameID: r.toUnicode() for r in latin['name'].names if r.nameID in (0, 13, 14)}
    name.names = []
    records = {
        0: keep.get(0, '') + ' Noto Sans CJK: Copyright 2014-2021 Adobe (http://www.adobe.com/).',
        1: FAMILY if weight in (400, 700) else f'{FAMILY} {style}',
        2: style if weight in (400, 700) else 'Regular',
        3: f'{FAMILY.replace(" ", "")}-{style};osmosis-build',
        4: f'{FAMILY} {style}',
        5: 'Version 1.000',
        6: f'{FAMILY.replace(" ", "")}-{style}',
        13: keep.get(13, 'SIL Open Font License 1.1'),
        14: keep.get(14, 'https://openfontlicense.org'),
        16: FAMILY,
        17: style,
    }
    for nid, text in records.items():
        name.setName(text, nid, 3, 1, 0x409)
    os2 = font['OS/2']
    os2.usWeightClass = weight
    os2.fsSelection = (os2.fsSelection & ~0b1100001) | (0b100000 if weight == 700 else 0b1000000)
    font['head'].macStyle = 1 if weight == 700 else 0


def latin(weight: int) -> TTFont:
    font = TTFont(HERE / 'src' / f'figtree-{weight}.ttf')
    freeze_tnum(font)
    return font


def build(weight: int, out: Path, charset: set[int]) -> None:
    lat = latin(weight)
    android = HERE / 'android' / f'osmosis-sans-{weight}.ttf'
    rename(lat, weight, lat)
    lat.save(android)

    cjk = instantiateVariableFont(noto_sc(), {'wght': weight})
    covered = set(lat.getBestCmap())
    options = subset.Options()
    options.layout_features = []
    options.hinting = False
    options.desubroutinize = True
    options.notdef_outline = True
    options.name_IDs = []
    sub = subset.Subsetter(options)
    sub.populate(unicodes=sorted(cp for cp in charset if cp not in covered and cp in cjk.getBestCmap()))
    sub.subset(cjk)
    to_truetype(cjk)
    # The merger can't combine these (variation stores, vertical metrics); the CJK half needs
    # none of them for horizontal text.
    for tag in ('GDEF', 'GSUB', 'GPOS', 'BASE', 'STAT', 'vhea', 'vmtx', 'HVAR', 'VVAR', 'MVAR'):
        if tag in cjk:
            del cjk[tag]
    # Figtree's vertical metrics win: the merger would take CJK's taller ones.
    cjk_path = out / f'.cjk-{weight}.ttf'
    lat_path = out / f'.lat-{weight}.ttf'
    cjk.save(cjk_path)
    lat.save(lat_path)
    merged = Merger().merge([str(lat_path), str(cjk_path)])
    cjk_path.unlink()
    lat_path.unlink()
    for table, fields in (('hhea', ('ascent', 'descent', 'lineGap')),
                          ('OS/2', ('sTypoAscender', 'sTypoDescender', 'sTypoLineGap',
                                    'usWinAscent', 'usWinDescent'))):
        for field in fields:
            setattr(merged[table], field, getattr(lat[table], field))
    rename(merged, weight, lat)
    path = out / f'osmosis-sans-{weight}.ttf'
    merged.save(path)
    print(f'{path.name}: {len(merged.getBestCmap())} code points, {path.stat().st_size // 1024} KB')


def check(charset: set[int]) -> None:
    """Titles and artists in the dev cache that the desktop subset can't draw."""
    state = Path(os.environ.get('XDG_STATE_HOME', Path.home() / '.local/state')) / 'osmosis-dev'
    db = state / 'cache.sqlite3'
    if not db.exists():
        print(f'no {db}; skipping the coverage check')
        return
    have = set(TTFont(HERE / 'desktop' / 'osmosis-sans-400.ttf').getBestCmap()) | charset
    texts = []
    with sqlite3.connect(f'file:{db}?mode=ro', uri=True) as conn:
        for (body,) in conn.execute('select body from responses'):
            for track in json.loads(body).get('tracks', []):
                texts.append(track.get('title') or '')
                texts.extend(track.get('artists') or [])
    missing = {c for text in texts for c in text if ord(c) > 0x7F and ord(c) not in have and not c.isspace()}
    print(f'{len(texts)} titles and artists checked, {len(missing)} characters outside the subset'
          + (f': {"".join(sorted(missing))}' if missing else ''))


def main() -> None:
    args = argparse.ArgumentParser()
    args.add_argument('--check', action='store_true')
    opts = args.parse_args()
    charset = desktop_charset()
    if not opts.check:
        for side in ('desktop', 'android'):
            (HERE / side).mkdir(exist_ok=True)
        for weight in WEIGHTS:
            build(weight, HERE / 'desktop', charset)
    check(charset)


if __name__ == '__main__':
    main()
