"""核对 assets/ 与 assets_embedded.rs 的一致性 —— CI 能不能构建取决于这个。

【为什么需要】
   上游 phira 的 .gitignore 排除了 /assets/font.ttf、/assets/background.jpg、
   /assets/res，因为上游是**运行时从 assets/ 目录读**。我们这个分支改成
   `include_bytes!` 内嵌（为了让 exe 单文件可运行），于是这些文件从
   「可选的运行时资源」变成了「编译期必需」。照抄上游的 .gitignore 会让
   新克隆 / CI 直接编译失败，报错还是最难懂的那种：

       error: couldn't read ../../assets/background.jpg: No such file or directory

   本脚本把这件事变成一条可执行的断言。

用法：
    python check-embedded-assets.py           # 全量核对
    python check-embedded-assets.py --quiet    # 只在有问题时输出
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GEN = os.path.join(ROOT, 'phira', 'src', 'assets_embedded.rs')
ASSETS = os.path.join(ROOT, 'assets')


def git_ignored(rel):
    r = subprocess.run(['git', 'check-ignore', rel], cwd=ROOT,
                       capture_output=True, text=True)
    return bool(r.stdout.strip())


def main():
    quiet = '--quiet' in sys.argv
    if not os.path.exists(GEN):
        print('❌ 找不到 ' + GEN + '（先跑 gen-embedded-assets.py）')
        return 1
    src = open(GEN, encoding='utf-8').read()
    need = sorted(set(re.findall(r'include_bytes!\("\.\./\.\./assets/([^"]+)"', src)))

    bad_ignored, bad_missing = [], []
    for f in need:
        rel = os.path.join('assets', f)
        if not os.path.exists(os.path.join(ROOT, rel)):
            bad_missing.append(f)
        elif git_ignored(rel):
            bad_ignored.append(f)

    ok = not bad_ignored and not bad_missing
    if not quiet or not ok:
        print(f'内嵌资源 {len(need)} 个')
    if bad_missing:
        print(f'\n❌ 磁盘上就没有（编译必败）{len(bad_missing)} 个：')
        for f in bad_missing:
            print('   ' + f)
    if bad_ignored:
        print(f'\n❌ 被 .gitignore 挡住（新克隆 / CI 必败）{len(bad_ignored)} 个：')
        for f in bad_ignored:
            print('   ' + f)
        print('\n   修法：把对应路径从 .gitignore 里去掉。')
        print('   上游 phira 是从 assets/ 目录运行时读，所以它排得掉；')
        print('   我们改成 include_bytes! 内嵌，这些文件就是编译期依赖。')
    if ok and not quiet:
        print('✅ 全部可入仓且磁盘在位')

    # 顺带报告没被内嵌的大文件 —— 它们不入仓也不影响构建
    extra = []
    for root, _, files in os.walk(ASSETS):
        for f in files:
            rel = os.path.relpath(os.path.join(root, f), ASSETS).replace(os.sep, '/')
            if rel not in need:
                extra.append(rel)
    if extra and not quiet:
        sizes = sorted((os.path.getsize(os.path.join(ASSETS, f)), f) for f in extra)
        total = sum(s for s, _ in sizes)
        print(f'\n未内嵌的 assets 文件 {len(extra)} 个，合计 {total / 1048576:.1f} MB'
              f'（不影响构建，可以不入仓）：')
        for s, f in reversed(sizes[:6]):
            print(f'   {s / 1024:8.1f} KB  {f}')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
