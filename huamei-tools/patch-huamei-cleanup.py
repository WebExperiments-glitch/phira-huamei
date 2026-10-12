"""清理 huamei.rs 里因登录链路替换而失效的东西，并把邮箱规则对齐服务端。

失效项（平台云认证专用，现在一条路都不走了）：
  · struct Session / OtpStarted / VerifyResp / Me
  · fn me()（拿平台 token 换用户信息）与 fn auth_base()
保留项（别误删）：
  · post_json  —— upload_play 还在用
  · bare_client —— post_json / account_call 都在用
  · readable_error —— post_json 的单测还在用
"""
import re
import sys

F = 'phira/src/huamei.rs'
s = open(F, encoding='utf-8').read()
orig = s

# ── 1. 删四个结构体（整块，含前面的 doc 注释）──
for name in ['Session', 'OtpStarted', 'VerifyResp', 'Me']:
    # 结构体定义（可能带 derive/doc 前缀）
    pat = re.compile(
        r'\n(?:(?:///[^\n]*\n|\s*\n)*)'          # 前面的文档注释与空行
        r'(?:#\[derive\([^\)]*\)\]\s*\n)?'
        r'pub struct ' + name + r'\b.*?\n\}\n',
        re.S)
    pat2 = re.compile(
        r'\n(?:(?:///[^\n]*\n|\s*\n)*)'
        r'(?:#\[derive\([^\)]*\)\]\s*\n)?'
        r'struct ' + name + r'\b.*?\n\}\n',
        re.S)
    new, n = pat.subn('\n', s)
    if n == 0:
        new, n = pat2.subn('\n', s)
    if n == 0:
        print(f'⚠ 没找到 struct {name}（可能已被删）')
    else:
        print(f'✅ 删除 struct {name}')
    s = new

# ── 2. 删 fn me() 与 fn auth_base() ──
for fn in ['me', 'auth_base']:
    pat = re.compile(
        r'\n(?:(?:///[^\n]*\n|\s*\n)*)'
        r'(?:pub )?async fn ' + fn + r'\(.*?\n\}\n',
        re.S)
    s, n = pat.subn('\n', s)
    print(('✅ 删除 fn ' + fn) if n else f'⚠ 没找到 fn {fn}')

# ── 3. 邮箱规则对齐服务端：放开 + 号别名与更多合法本地字符 ──
old_local = """    if let Some(c) = local
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "「{shown}」：@ 前面不能有「{c}」—— 只允许英文、数字、点、下划线、减号。\\
             ⚠ 不支持加号别名（name+tag@…），请用主邮箱地址"
        ));
    }"""
new_local = """    /* ⚠ 允许的字符集**照抄服务端**（`lib/hm-accounts.mjs` 的 validateEmail）：
       英文、数字、以及 . ! # $ % & ' * + / = ? ^ _ ` { | } ~ -
       以前禁 `+` 是**平台**的限制（它的正则不收），现在我们自己存账号，
       没有理由继承。两边规则必须一致，否则会出现「本地说合法、服务端说非法」。 */
    if let Some(c) = local
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~-".contains(c)))
    {
        return Err(format!(
            "「{shown}」：@ 前面不能有「{c}」—— 只允许英文、数字和 . ! # $ % & ' * + / = ? ^ _ ` {{ | }} ~ -"
        ));
    }"""
if old_local in s:
    s = s.replace(old_local, new_local, 1)
    print('✅ 邮箱本地部分规则已对齐服务端（放开 + 别名）')
else:
    print('⚠ 没匹配到邮箱本地部分规则，可能已被改过')

# ── 4. validate_email 的 doc 注释还在讲平台的正则，改掉 ──
s = s.replace(
    '/// 服务端（`.cloud/auth/v1/verification`）对一个不合法邮箱的回应是：',
    '/// 【原来的写法】这里贴的是平台云认证对一个不合法邮箱的回应。'
    '换成自建账号后服务端换了，但「提前拦下来、指出哪个字符不对」这件事不变，',
    1)
s = s.replace(
    '/// 这段 protobuf 报文直接糊进对话框，用户只会看到"登录失败"。',
    '/// 规则仍以 `phm-web/lib/hm-accounts.mjs` 的 validateEmail 为准。',
    1)

open(F, 'w', encoding='utf-8', newline='\n').write(s)
print(f'\n文件 {len(orig)} → {len(s)} 字符')
