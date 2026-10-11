#!/usr/bin/env python3
"""
把 `assets/` 嵌进可执行文件 —— 生成 `phira/src/assets_embedded.rs`。

【为什么需要】
  Phira 用 macroquad 的 `load_file("assets/...")` 读资源，而 macroquad 这个
  fork **没有**自定义文件加载钩子（只有一个 `set_pc_assets_folder`）。
  于是 exe 必须和 `assets/` 目录待在一起 —— 用户双击单独的 exe 就报
  `Couldn't load file assets\\phigros.ttf: NotFound`。

  这个脚本把 77 个资源文件用 `include_bytes!` 编进二进制，配合
  `phira/src/lib.rs` 的 `ensure_embedded_assets()`：**首次运行**时如果
  exe 旁边没有 `assets/`，就把它们释放出来。

【为什么是「释放」而不是「内存里读」】
  因为没法拦截 `load_file`（见上）。释放到磁盘再照常读，是最小改动、
  也是唯一能覆盖全部 77 个读取点（贴图/音频/字体/谱包）的做法。

用法（必须在仓库根目录跑）：
  python huamei-tools/gen-embedded-assets.py

⚠ 改动 assets/ 之后要重跑，否则嵌进去的还是旧的。
⚠ 生成的 .rs 是**产物**，别手改。
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, 'assets')
OUT = os.path.join(ROOT, 'phira', 'src', 'assets_embedded.rs')

# 这些不嵌：
#   res/ —— 运行时由用户导入的 respack 目录（上游 gitignore 的）
SKIP_DIRS = {'res'}


def collect():
    out = []
    for dirpath, dirnames, filenames in os.walk(ASSETS):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for fn in sorted(filenames):
            full = os.path.join(dirpath, fn)
            rel = os.path.relpath(full, ASSETS).replace(os.sep, '/')
            out.append((rel, full))
    out.sort()
    return out


def main():
    if not os.path.isdir(ASSETS):
        print('✗ 找不到 ' + ASSETS)
        return 1
    files = collect()
    total = sum(os.path.getsize(p) for _, p in files)

    lines = [
        '//! 【自动生成，别手改】',
        '//!',
        '//! 由 `huamei-tools/gen-embedded-assets.py` 生成。',
        '//! 把 `assets/` 下的全部资源编进二进制，让 exe 可以单文件分发 ——',
        '//! 首次运行时由 `crate::ensure_embedded_assets()` 释放到 exe 旁边。',
        '//!',
        '//! 【为什么需要这样】',
        '//!   Phira 通过 macroquad 的 `load_file("assets/...")` 读资源，而这个',
        '//!   macroquad fork 没有自定义文件加载钩子，拦不住那些读取点。',
        '//!   所以只能「嵌进去 → 落盘 → 照常读」。',
        '//!',
        '//! 路径是相对**本文件**（`phira/src/`）的，`../../assets` = 仓库根的 assets/。',
        '',
        '/// (相对 assets/ 的路径, 文件内容)',
        'pub static ASSETS: &[(&str, &[u8])] = &[',
    ]
    for rel, full in files:
        lines.append('    ("%s", include_bytes!("../../assets/%s")),' % (rel, rel))
    lines += [
        '];',
        '',
    ]
    txt = '\n'.join(lines)
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, 'w', encoding='utf-8', newline='\n') as f:
        f.write(txt)

    print('[embedded] %d 个文件 / %.1f MB → %s' %
          (len(files), total / 1048576, os.path.relpath(OUT, ROOT)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
