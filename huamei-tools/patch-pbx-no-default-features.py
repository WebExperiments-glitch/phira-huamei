"""给 phira.xcodeproj 的 Cargo Build 脚本加 --no-default-features。

【为什么】
   上游的脚本是裸的 `cargo build --bin phira-main --target ...`，
   这会带上 phira-main 的 default feature —— `video`，也就是 prpr-avc，
   而 prpr-avc 的 build.rs 会去**下载 FFmpeg 静态库**：
     Windows 上就是这么挂的（invalid peer certificate: UnknownIssuer）。
   iOS 构建同样会去下 macos/ios 的那一份，白白多几分钟还多一个失败点。
   我们从头就不需要视频背景，所以显式关掉。

【怎么改】
   只动那一行 cargo 命令，其余字节不动。改完会打印前后差异供核对。
"""
import re
import sys

PBX = 'phira.xcodeproj/project.pbxproj'

s = open(PBX, encoding='utf-8').read()

# ⚠ pbxproj 里 shellScript 是"带引号 + 反斜杠转义"的字符串，所以源码里的
#   " 在文件里是 \"。匹配串必须按**文件里的原样**写，按 Rust 源码的样貌
#   写是找不到的（第一版就栽在这）。
OLD = 'cargo build --bin phira-main --target \\"$RUST_TARGET\\" $PROFILE_FLAG'
NEW = ('cargo build --bin phira-main --no-default-features '
       '--target \\"$RUST_TARGET\\" $PROFILE_FLAG')

if OLD not in s:
    if NEW in s:
        print('✅ 已经改过了，无需重复')
        sys.exit(0)
    print('❌ 在 pbxproj 里找不到预期的 cargo 命令行：')
    print('   ' + OLD)
    print('   上游可能改了写法，请人工核对 ' + PBX)
    sys.exit(1)

# pbxproj 里 shellScript 是带引号 + \n 转义的字符串，直接做等价替换即可
s2 = s.replace(OLD, NEW, 1)
open(PBX, 'w', encoding='utf-8', newline='').write(s2)

print('✅ 已改：')
print('   - ' + OLD)
print('   + ' + NEW)
print()
print('   附带说明（写进注释，免得下次又被"优化"回去）：')
print('   --no-default-features 关掉 video/prpr-avc → 不再下载 FFmpeg 静态库')

# 顺手确认没有别处也在跑裸 cargo build
others = re.findall(r'cargo build[^"\\]*', s2)
print()
print('pbxproj 里全部 cargo 命令：')
for o in others:
    print('   ' + o)
