"""改 phira.xcodeproj 的 PRODUCT_BUNDLE_IDENTIFIER。

【为什么必须改】
   上游写的是 `org.flos.phira` —— 跟 App Store 上那个 Phira **同一个
   bundle ID**。iOS 按 bundle ID 认应用，同一个 ID 就是同一个应用。
   拿侧载包装上去，用户机器里的正版 Phira 会被**顶掉**（数据、存档一起没）。

   这不是理论风险：目标用户基本都在 App Store 装着 Phira。

【改成什么】
   `app.huamei.phira` —— 我们自己的命名空间，和上游、和别的 fork 都不撞。
   侧载包不要求 ID 和任何账号绑定，所以随便取一个独有的就行。
"""
import sys

PBX = 'phira.xcodeproj/project.pbxproj'
OLD = 'PRODUCT_BUNDLE_IDENTIFIER = org.flos.phira;'
NEW = 'PRODUCT_BUNDLE_IDENTIFIER = app.huamei.phira;'

s = open(PBX, encoding='utf-8').read()
if NEW in s:
    print('✅ 已经是 app.huamei.phira，无需重复')
    sys.exit(0)
if OLD not in s:
    print('❌ 找不到 ' + OLD)
    print('   上游可能改了写法，请人工核对 ' + PBX)
    sys.exit(1)

open(PBX, 'w', encoding='utf-8', newline='').write(s.replace(OLD, NEW, 1))
print('✅ PRODUCT_BUNDLE_IDENTIFIER: org.flos.phira → app.huamei.phira')
print('   理由：同一个 ID 会被 iOS 当成同一个应用，侧载会顶掉用户的正版 Phira')
