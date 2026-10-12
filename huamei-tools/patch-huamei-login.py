"""把 huamei.rs 的登录链路从「平台验证码」换成「自建账号 邮箱+密码」。

做三件事：
  ① 删掉 send_otp / verify_otp / sign_in_password（平台云认证那套）
  ② 在同一位置插入 register / login（打我们自己的 /api/hm/*）
  ③ 删掉 start_login / finish_login（验证码两步状态机）

⚠ 用行号区间 + 断言，不用大字符串匹配 —— 那玩意儿在这份文件里太容易失配。
"""
import sys

F = 'phira/src/huamei.rs'
lines = open(F, encoding='utf-8').read().split('\n')

def find(pred, what):
    for i, l in enumerate(lines):
        if pred(l):
            return i
    print(f'❌ 找不到 {what}')
    sys.exit(1)

# ── ① 定位三个要删的函数 ──
i_send = find(lambda l: l.startswith('pub async fn send_otp('), 'send_otp')
i_verify = find(lambda l: l.startswith('pub async fn verify_otp('), 'verify_otp')
i_signin = find(lambda l: l.startswith('pub async fn sign_in_password('), 'sign_in_password')
i_me = find(lambda l: l.startswith('pub async fn me('), 'me')
i_start = find(lambda l: l.startswith('pub async fn start_login('), 'start_login')
i_finish = find(lambda l: l.startswith('pub async fn finish_login('), 'finish_login')
i_upload = find(lambda l: l.startswith('pub async fn upload_play('), 'upload_play')

assert i_send < i_verify < i_signin < i_me, (i_send, i_verify, i_signin, i_me)
assert i_start < i_finish < i_upload, (i_start, i_finish, i_upload)

NEW_ACCOUNT_API = '''/// 自建账号：注册 / 登录。
///
/// 【为什么不再是平台云认证】
///   原先走 `.cloud/auth/v1`，每次登录都要邮箱 + 验证码；而且 SDK 明确拒绝
///   「只有用户名+密码」的注册（`signUp requires a verificationToken`），
///   所以「填完邮箱就填密码、然后完成注册」在平台侧做不到。
///   现在账号在我们自己手里（`phm-web/lib/hm-accounts.mjs`）：
///     注册 = 邮箱 + 密码，无验证码
///     登录 = 邮箱 + 密码（邮箱由客户端记住，用户只输密码）
///
/// 【错误码】服务端回**稳定错误码**，客户端翻译成人话。
///   email_invalid / password_invalid → 输入不合法，改一下就能好
///   email_taken                      → 注册时邮箱已被占用
///   bad_credentials                  → 登录时邮箱或密码不对
///                                      （刻意不区分「账号不存在」和「密码错」，
///                                       否则这个接口成了账号枚举器）
#[derive(Debug, Deserialize)]
struct AccountResp {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

/// 调 `/api/hm/register` 或 `/api/hm/login`。
/// 成功返回 token，失败返回 `(错误码, 详情)`。
///
/// ⚠ 刻意不复用 `post_json`：它把错误糊成一句字符串，这里需要**错误码**
///   才能给出「该改输入还是该重试」的区分。
async fn account_call(
    step: &str,
    path: &str,
    email: &str,
    password: &str,
) -> std::result::Result<String, (String, Option<String>)> {
    let body = serde_json::json!({ "email": email, "password": password });
    let resp = bare_client()
        .post(format!("{}/{}", hm_base(), path))
        .json(&body)
        .send()
        .await
        .map_err(|e| ("network".to_string(), Some(e.to_string())))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    log_debug_line(&format!(
        "[step] {step} → HTTP {status} ok body={}",
        text.chars().take(200).collect::<String>()
    ));
    let r: AccountResp = serde_json::from_str(&text).map_err(|_| {
        ("bad_response".to_string(), Some(text.chars().take(200).collect()))
    })?;
    if !status.is_success() || !r.ok {
        let code = r.error.unwrap_or_else(|| format!("http_{}", status.as_u16()));
        return Err((code, r.detail));
    }
    r.token
        .filter(|it| !it.is_empty())
        .ok_or_else(|| ("no_token".to_string(), None))
}

/// 把服务端错误码翻成人话。
///
/// ⚠ `detail` **只在输入不合法时透出** —— 那是帮用户改对的提示。
///   凭据类错误的 detail 可能是数据库原始报文，不该往界面上糊。
fn account_error(step: &str, code: &str, detail: Option<&str>) -> anyhow::Error {
    let msg = match code {
        "email_invalid" => format!(
            "邮箱格式不对：{}",
            detail.unwrap_or("请检查是否只有一个 @、域名有没有带点")
        ),
        "password_invalid" => format!(
            "密码不符合要求：{}",
            detail.unwrap_or("至少 8 位")
        ),
        "email_taken" => "这个邮箱已经注册过了 —— 请直接用密码登录".to_string(),
        "bad_credentials" => "邮箱或密码不对。如果这是新账号，请先退出当前账号再注册".to_string(),
        "unavailable" => "服务端没配好，暂时不能注册/登录".to_string(),
        "store_failed" => "服务端出错了，请稍后再试".to_string(),
        "network" => "连不上服务器，请检查网络".to_string(),
        "bad_response" => "服务端返回了看不懂的内容，请稍后再试".to_string(),
        "no_token" => "服务端没给 token，请重试".to_string(),
        other => format!("{step}失败（{other}）"),
    };
    anyhow::anyhow!(msg)
}

/// 注册：邮箱 + 密码，**不需要验证码**。
pub async fn register(email: &str, password: &str) -> Result<Step> {
    let token = account_call("注册", "register", email, password)
        .await
        .map_err(|(code, detail)| account_error("注册", &code, detail.as_deref()))?;
    Ok(Step::LoggedIn {
        token,
        email: Some(email.to_owned()),
    })
}

/// 登录：邮箱 + 密码。邮箱由客户端记住，用户只输密码。
pub async fn login(email: &str, password: &str) -> Result<Step> {
    let token = account_call("登录", "login", email, password)
        .await
        .map_err(|(code, detail)| account_error("登录", &code, detail.as_deref()))?;
    Ok(Step::LoggedIn {
        token,
        email: Some(email.to_owned()),
    })
}
'''

NEW_LOGIN_API = '''/// 密码格式校验（本地先拦一道，别什么都往服务端送）。
///
/// 规则与 `phm-web/lib/hm-accounts.mjs` 的 `validatePassword` **一致** ——
/// 两边不一致会出现「本地说合法、服务端说非法」的怪事。
pub fn validate_password(pw: &str) -> std::result::Result<(), String> {
    if pw.is_empty() {
        return Err("密码不能为空".into());
    }
    if pw.chars().count() < 8 {
        return Err(format!("密码至少 8 位（当前 {} 位）", pw.chars().count()));
    }
    if pw.chars().count() > 128 {
        return Err("密码太长（最多 128 位）".into());
    }
    if pw.chars().all(|c| c.is_whitespace()) {
        return Err("密码不能全是空白字符".into());
    }
    Ok(())
}
'''

# ── ② 拼装新文件 ──
out = []
out.extend(lines[:i_send])                       # 到 send_otp 的 doc 注释之前
out.extend(NEW_ACCOUNT_API.split('\n'))
out.extend(lines[i_me:i_start])                  # me() / current_token() 原样保留
out.extend(NEW_LOGIN_API.split('\n'))
out.extend(lines[i_upload:])                     # 成绩上报起，原样保留

open(F, 'w', encoding='utf-8', newline='\n').write('\n'.join(out))
print('✅ huamei.rs 登录链路已替换')
print(f'   删除: send_otp / verify_otp / sign_in_password / start_login / finish_login')
print(f'   新增: register / login / account_call / account_error / validate_password')
