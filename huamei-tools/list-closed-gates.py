"""列出所有 `cfg(closed)` 门禁，带上下文 —— 看清开源构建里被关掉了什么。

用法： python list-closed-gates.py [每个站点的上下行数]
"""
import os
import re
import sys

ROOTS = ['phira/src', 'prpr/src', 'phira-main/src']
CTX = int(sys.argv[1]) if len(sys.argv) > 1 else 6

sites = []
for root in ROOTS:
    for dirpath, _, files in os.walk(root):
        for f in files:
            if not f.endswith('.rs'):
                continue
            p = os.path.join(dirpath, f)
            lines = open(p, encoding='utf-8', errors='replace').read().split('\n')
            for i, l in enumerate(lines):
                if 'cfg(closed)' in l or 'cfg(not(closed))' in l:
                    if l.strip().startswith('//') or l.strip().startswith('///'):
                        continue
                    sites.append((p, i + 1, lines))

print(f'共 {len(sites)} 处门禁\n')
for p, ln, lines in sites:
    kind = '闭源专有' if 'not(closed)' not in lines[ln - 1] else '开源专有'
    print(f'═══ {p}:{ln}  [{kind}] ═══')
    a = max(0, ln - 1 - CTX)
    b = min(len(lines), ln - 1 + CTX + 1)
    for j in range(a, b):
        mark = '»' if j == ln - 1 else ' '
        print(f'{mark} {j + 1:>4} | {lines[j]}')
    print()
