#!/usr/bin/env python3
"""
huamei 客户端 —— 运行资源准备。

上游 `phira/.gitignore` 把两个**启动必需**的资源排除了，所以从源码构建
必须自己补上，否则 `phira/src/lib.rs:227` 加载 `font.ttf` 会直接失败：

    /assets/font.ttf          主 UI 字体（必须覆盖 CJK）
    /assets/background.jpg    默认背景图

本脚本幂等：已存在的文件不覆盖。

⚠ **字体的授权问题（分发前必须解决）**
  Windows 自带的 Deng.ttf / simhei.ttf **不可再分发**（微软字体授权不允许）。
  所以本脚本优先下载 **Noto Sans SC（SIL OFL 1.1）**，它是可随 GPL-3.0 分发的。
  只有下载全部失败时才退到系统字体，并打印醒目警告 —— 那种产物**只能自己用**。

用法：
  python huamei-tools/prepare-assets.py            # 补缺的资源
  python huamei-tools/prepare-assets.py --force    # 强制重下字体
"""
import argparse
import os
import shutil
import subprocess
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
FONT = os.path.join(ASSETS, "font.ttf")
BG = os.path.join(ASSETS, "background.jpg")

# Noto Sans SC —— SIL OFL 1.1，与 GPL-3.0 兼容，可分发
GH = ("https://github.com/notofonts/noto-cjk/raw/main/"
      "Sans/SubsetOTF/SC/NotoSansSC-Regular.otf")
FONT_MIRRORS = [
    ("ghfast",     "https://ghfast.top/" + GH),
    ("gh-proxy",   "https://gh-proxy.com/" + GH),
    ("ghproxy.net", "https://ghproxy.net/" + GH),
    ("jsdelivr",   "https://fastly.jsdelivr.net/gh/notofonts/noto-cjk@main/"
                   "Sans/SubsetOTF/SC/NotoSansSC-Regular.otf"),
]

# 系统字体兜底（**不可分发**，仅本地跑通用）
SYSTEM_FONTS = [
    r"C:\Windows\Fonts\Deng.ttf",      # 等线，现代无衬线
    r"C:\Windows\Fonts\simhei.ttf",    # 黑体
]


def log(msg):
    print(msg, flush=True)


def download(url, dst, timeout=120):
    """优先用 curl（能跟随镜像跳转、支持断点），没有就退回 urllib"""
    if shutil.which("curl"):
        r = subprocess.run(
            ["curl", "-L", "-s", "--retry", "2", "--connect-timeout", "15",
             "--max-time", str(timeout), "-o", dst, url],
            capture_output=True)
        if r.returncode == 0 and os.path.exists(dst) and os.path.getsize(dst) > 1 << 20:
            return True
        return False
    req = urllib.request.Request(url, headers={"User-Agent": "curl/8"})
    with urllib.request.urlopen(req, timeout=timeout) as resp, open(dst, "wb") as f:
        shutil.copyfileobj(resp, f)
    return os.path.getsize(dst) > 1 << 20


def replace_retry(src, dst, tries=8):
    """Windows 上 `os.replace` 会撞 Defender 实时扫描 —— 新写出来的文件
    会被短暂占用，报 WinError 32（另一个程序正在使用此文件）。
    退避重试几乎总能过去。"""
    import time
    for i in range(tries):
        try:
            os.replace(src, dst)
            return True
        except PermissionError:
            if i == tries - 1:
                raise
            time.sleep(0.4 * (i + 1))
    return False


def validate_font(path):
    """字体必须能解析、且真的有 CJK 字形"""
    try:
        from fontTools.ttLib import TTFont
    except ImportError:
        log("  (未装 fontTools，跳过校验 —— 建议 pip install fonttools)")
        return True
    try:
        f = TTFont(path, fontNumber=0, lazy=True)
        cmap = f.getBestCmap()
    except Exception as e:                             # noqa: BLE001
        log("  ✗ 字体无法解析：%s" % e)
        return False
    # 抽查几个常用汉字
    probe = "的一是不了在人有我重生谱面"
    missing = [c for c in probe if ord(c) not in cmap]
    if missing:
        log("  ✗ 缺字形：%s" % "".join(missing))
        return False
    outlines = "CFF" if "CFF " in f else ("glyf" if "glyf" in f else "?")
    log("  ✓ 字形 %d 个，轮廓 %s" % (len(cmap), outlines))
    return True


def make_background():
    """深色对角渐变 —— 与站点同一个暖色温层"""
    try:
        from PIL import Image
    except ImportError:
        log("  (未装 Pillow，用纯色占位)")
        return False
    w, h = 1920, 1080
    # #0e0d0b（站点 --bg）→ #241d14（暖褐），对角插值
    c0 = (14, 13, 11)
    c1 = (36, 29, 20)
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w // 4):
            t = ((x / (w // 4)) + (y / h)) / 2
            c = tuple(int(c0[i] + (c1[i] - c0[i]) * t) for i in range(3))
            px[x, y] = c
            px[w - 1 - x, y] = c
    img.save(BG, "JPEG", quality=88)
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true", help="强制重下字体")
    args = ap.parse_args()

    os.makedirs(ASSETS, exist_ok=True)
    dirty = False

    # ── 字体 ──
    if os.path.exists(FONT) and not args.force:
        log("[font] 已存在，跳过（%d KB）" % (os.path.getsize(FONT) // 1024))
    else:
        tmp = FONT + ".part"
        ok = False
        for name, url in FONT_MIRRORS:
            log("[font] 尝试 %s …" % name)
            try:
                if download(url, tmp):
                    if validate_font(tmp):
                        replace_retry(tmp, FONT)
                        log("[font] ✓ Noto Sans SC（SIL OFL 1.1，可分发）→ %s" % FONT)
                        ok = True
                        break
                    else:
                        log("[font] %s 下载的字体校验没过" % name)
            except Exception as e:                     # noqa: BLE001
                log("[font] %s 失败：%s" % (name, str(e)[:80]))
            if os.path.exists(tmp):
                try:
                    os.remove(tmp)
                except PermissionError:
                    log("[font] （清理 %s 失败，无害，下次会覆盖）" % os.path.basename(tmp))

        if not ok:
            log("")
            log("⚠️  ══════════════════════════════════════════════════════")
            log("⚠️   所有镜像都失败，退回**系统字体**。")
            log("⚠️   微软字体**不可再分发** —— 这个构建只能你自己用，")
            log("⚠️   绝对不能把 exe / assets 发出去。")
            log("⚠️   ══════════════════════════════════════════════════════")
            for src in SYSTEM_FONTS:
                if os.path.exists(src):
                    shutil.copyfile(src, FONT)
                    log("[font] 用了 %s（%.1f MB）" % (src, os.path.getsize(FONT) / 1048576))
                    ok = True
                    break
            if not ok:
                log("[font] ✗ 找不到任何可用字体，游戏起不来")
                return 1
        dirty = True

    # ── 背景图 ──
    if os.path.exists(BG):
        log("[bg]   已存在，跳过")
    else:
        log("[bg]   生成 1920×1080 暖色对角渐变 …")
        make_background()
        log("[bg]   ✓ %s" % BG)
        dirty = True

    log("")
    log("资源就绪。运行：")
    log("  cd %s" % ROOT)
    log("  ./target/release/phira-main.exe      # ⚠ 必须在项目根目录跑")
    return 0 if not dirty else 0


if __name__ == "__main__":
    sys.exit(main())
