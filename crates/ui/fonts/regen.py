# 生成标题字体与拉丁三件。入口是 `just font-title-subset`,在 fonttools 的
# nix-shell 里跑,不要直接 python 执行(依赖不在系统环境里)。
#
# 标题字符集的唯一真相是 ../tests/fonts.rs 里的 CJK_TITLES:本脚本从那里
# 解析字符串字面量,所以「测试红了」与「子集裁少了」永远指向同一处修改。
#
# 思源宋体来自系统的 Noto Serif CJK SC 变体字体(同一设计的 Google 发行名),
# 钉在 wght=900(Source Han Serif 的 Heavy 档),再裁到标题字符集。
# 拉丁三件从 Google Fonts 仓库下载后放在 --latin-dir,Figtree 是变体字体,
# 钉出 400/600/700 三个静态实例。name-IDs 13/14 是 OFL 许可声明,随子集分发。

import re
import subprocess
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTCollection, TTFont
from fontTools.varLib.instancer import instantiateVariableFont

HERE = Path(__file__).parent
TESTS = HERE.parent / 'tests' / 'fonts.rs'


def title_chars() -> str:
    body = TESTS.read_text(encoding='utf-8')
    block = re.search(r'CJK_TITLES[^=]*=\s*&\[(.*?)\];', body, re.S)
    if not block:
        sys.exit('tests/fonts.rs 里找不到 CJK_TITLES')
    literals = re.findall(r'"([^"]+)"', block.group(1))
    chars = sorted(set(''.join(literals)))
    if not chars:
        sys.exit('CJK_TITLES 解析出来是空的')
    return ''.join(chars)


def find_serif_sc(ttc_path: str) -> TTFont:
    for font in TTCollection(ttc_path).fonts:
        name = font['name'].getDebugName(16) or font['name'].getDebugName(1)
        if name == 'Noto Serif CJK SC':
            return font
    sys.exit(f'{ttc_path} 里没有 Noto Serif CJK SC')


def build_title_subset() -> None:
    ttc = subprocess.run(
        ['fc-match', '-f', '%{file}', 'Noto Serif CJK SC'],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    font = find_serif_sc(ttc)
    instantiateVariableFont(font, {'wght': 900}, inplace=True)

    options = subset.Options()
    options.text = title_chars()
    options.layout_features = []
    options.hinting = False
    options.desubroutinize = True
    options.notdef_outline = True
    options.name_IDs = [0, 1, 2, 3, 4, 6, 13, 14, 16, 17]
    subsetter = subset.Subsetter(options)
    subsetter.populate(text=options.text)
    subsetter.subset(font)
    out = HERE / 'cjk-title-subset.otf'
    font.save(out)
    print(out, out.stat().st_size)


def build_latin(latin_dir: Path) -> None:
    # 不改 name 表:三个静态实例共用族名 Figtree,靠 OS/2 字重类区分,
    # 界面上 font-family "Figtree" + font-weight 就能选中对应实例。
    for weight in (400, 600, 700):
        inst = TTFont(latin_dir / 'Figtree[wght].ttf')
        instantiateVariableFont(inst, {'wght': weight}, inplace=True)
        out = HERE / f'figtree-{weight}.ttf'
        inst.save(out)
        print(out, out.stat().st_size)

    for src, dst in (
        ('Caprasimo-Regular.ttf', 'caprasimo.ttf'),
        ('DMMono-Regular.ttf', 'dm-mono.ttf'),
    ):
        data = (latin_dir / src).read_bytes()
        (HERE / dst).write_bytes(data)
        print(HERE / dst, len(data))


# 两半各自独立:无参数只重裁标题子集(改标题后的常规路径),
# 带一个目录参数只重做拉丁三件(目录里放三个 Google Fonts 源文件)。
if __name__ == '__main__':
    if len(sys.argv) > 1:
        build_latin(Path(sys.argv[1]))
    else:
        build_title_subset()
