//! huamei 客户端 —— 账号与成绩上报。
//!
//! 【为什么不复用 Phira 的 `Client`】
//!   `Client` 的基址是 `API_URL`（我们的取谱代理），鉴权头是 Phira 的 Bearer token。
//!   huamei 的账号与上报走**另外两条路**：`/.cloud/auth/v1/*`（平台托管鉴权）
//!   与 `/api/hm/*`（我们自己的服务端）。基址、请求头都不同，混用会串号。
//!
//! 【原生客户端为什么能直接用云鉴权】
//!   实测：带 `x-wb-webapp-access-key: <publishableKey>` 请求
//!   `GET /.cloud/auth/v1/user/me` 返回
//!   `{"code":"UNAUTHENTICATED",...,"authorization not found"}` ——
//!   即**密钥被接受**，只差用户 token。不带这个头才返回 `invalid_client`。
//!   ⚠ 头名是 `x-wb-webapp-access-key`，**不是** `apikey`（后者会 invalid_client）。
//!
//! 【接口形状的来源】
//!   从 `@tencent-ai/workbuddy-cloud-sdk` 的 IIFE 构建里读出来的（minified）。
//!   路径表：`signup` / `signin` / `verification` / `verification/verify` /
//!   `token` / `user/me` / `user/signout`，前缀 `/.cloud/auth/v1`。
//!   ⚠ 别照抄网上 Supabase 的 `/auth/v1/token?grant_type=password` —— 形状不一样。
//!
//! 契约见 `docs/规范/数据飞轮契约.md`。

use anyhow::{bail, Context, Result};
use prpr::huamei::HuameiPayload;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{
    client::{basic_client_builder, HUAMEI_CLIENT_VER, HUAMEI_PUBLISHABLE_KEY, HUAMEI_SITE},
    get_data,
};

/// 云鉴权要求的应用标识头。**名字必须是这个**。
const KEY_HEADER: &str = "x-wb-webapp-access-key";

// 基址从 `HUAMEI_SITE` 派生（`const` 里没法拼接运行时才知道的串，用函数）。
// `/.cloud/*` 走平台边缘，`/api/hm/*` 走我们自己的 server.mjs。
fn auth_base() -> String {
    format!("{HUAMEI_SITE}/.cloud/auth/v1")
}

fn hm_base() -> String {
    format!("{HUAMEI_SITE}/api/hm")
}

/// 登录会话。字段名跟服务端 snake_case 一致。
#[derive(Clone, Debug, Deserialize)]
pub struct Session {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OtpStarted {
    #[serde(default)]
    pub verification_id: String,
    #[serde(default)]
    pub is_user: bool,
}

#[derive(Debug, Deserialize)]
struct VerifyResp {
    #[serde(default)]
    verification_token: String,
}

#[derive(Debug, Deserialize)]
pub struct Me {
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub play_id: Option<i64>,
}

#[derive(Serialize)]
struct UploadBody<'a> {
    client_ver: &'a str,
    judge_ver: u8,
    chart_id: i32,
    chart_level: &'a str,
    chart_digest: Option<&'a str>,
    chart_notes: u32,
    score: u32,
    accuracy: f64,
    max_combo: u32,
    counts: [u32; 4],
    speed: f32,
    mods: i32,
    keyboard: bool,
    played_at: i64,
    judge: &'a str,
}

fn bare_client() -> reqwest::Client {
    let mut headers = HeaderMap::new();
    headers.insert(
        KEY_HEADER,
        HeaderValue::from_static(HUAMEI_PUBLISHABLE_KEY),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    basic_client_builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

/// 把服务端的错误响应压成一句能看的话。
///
/// 云服务回的是 protobuf 风格的 JSON，例如
/// ```json
/// {"code":"INVALID_ARGUMENT","error":"invalid_argument","error_code":3,
///  "error_description":"invalid SendVerificationCodeRequest.Email: ..."}
/// ```
/// 原样 `{:?}` 出去会在对话框里堆一大坨，用户只会记住"登录失败"。
fn readable_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        for k in ["error_description", "message", "error"] {
            if let Some(s) = v.get(k).and_then(|it| it.as_str()) {
                let s = s.trim();
                if !s.is_empty() {
                    return s.chars().take(220).collect();
                }
            }
        }
    }
    text.chars().take(220).collect()
}

async fn post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    url: &str,
    body: &T,
    token: Option<&str>,
) -> Result<R> {
    let mut req = bare_client().post(url).json(body);
    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = req.send().await.with_context(|| "网络请求失败（检查网络/代理）".to_string())?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{}（HTTP {status}）", readable_error(&text));
    }
    serde_json::from_str::<R>(&text).with_context(|| "服务端返回的不是预期 JSON".to_string())
}

// ══════════════════════════════════════════════════════════════
// 登录
// ══════════════════════════════════════════════════════════════

/// 第一步：发邮箱验证码。返回 `verification_id`（下一步要用）。
///
/// 对应 SDK 的 `auth.sendOtp({ email })`。
pub async fn send_otp(email: &str) -> Result<OtpStarted> {
    let body = serde_json::json!({ "email": email, "usage": "email" });
    post_json::<_, OtpStarted>(&format!("{}/verification", auth_base()), &body, None).await
}

/// 第二步：验证码换会话。
///
/// `is_existing` 来自第一步的 `is_user` —— 新老用户走**不同的端点**：
///   老用户 `POST /signin`  body `{username, verification_token}`
///   新用户 `POST /signup`  body `{email, password, verification_token}`
/// （新用户必须带密码，否则「用没有密码的账号永远登不进去」。）
pub async fn verify_otp(
    verification_id: &str,
    code: &str,
    email: &str,
    is_existing: bool,
    password: Option<&str>,
) -> Result<Session> {
    let vbody = serde_json::json!({
        "verification_id": verification_id,
        "verification_code": code,
    });
    let v: VerifyResp =
        post_json(&format!("{}/verification/verify", auth_base()), &vbody, None).await?;
    if v.verification_token.is_empty() {
        bail!("服务端没有返回 verification_token");
    }

    if is_existing {
        let body = serde_json::json!({
            "username": email,
            "verification_token": v.verification_token,
        });
        post_json(&format!("{}/signin", auth_base()), &body, None).await
    } else {
        let body = serde_json::json!({
            "email": email,
            "password": password.unwrap_or(""),
            "verification_token": v.verification_token,
        });
        post_json(&format!("{}/signup", auth_base()), &body, None).await
    }
}

/// 密码登录。对应 SDK 的 `auth.signInWithPassword`。
pub async fn sign_in_password(email: &str, password: &str) -> Result<Session> {
    let body = serde_json::json!({ "username": email, "password": password });
    post_json(&format!("{}/signin", auth_base()), &body, None).await
}

/// 校验当前 token 是否有效，返回用户信息。
pub async fn me(token: &str) -> Result<Me> {
    let resp = bare_client()
        .get(format!("{}/user/me", auth_base()))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token 无效（{status}）");
    }
    serde_json::from_str(&text).context("user/me 返回的不是预期 JSON")
}

/// 当前登录用的 token。优先用已保存的会话，其次看 `HUAMEI_TOKEN` 环境变量
/// （方便在还没做登录界面时先把上报链路测通）。
pub fn current_token() -> Option<String> {
    get_data()
        .huamei_token
        .clone()
        .filter(|it| !it.is_empty())
        .or_else(|| std::env::var("HUAMEI_TOKEN").ok().filter(|it| !it.is_empty()))
}

// ══════════════════════════════════════════════════════════════
// 登录流程的状态机（由设置页驱动）
// ══════════════════════════════════════════════════════════════

/// 一次登录流程里，异步任务完成后交给界面的「下一步」。
///
/// ⚠ 刻意**不在这里存 token** —— 落盘要在同步上下文里做。
///   在 async 块里拿 `get_data_mut()` 会得到一个跨 await 持有的
///   `&'static mut Data`，那既不好过 Send 检查，也容易把借用拖得很长。
///   所以任务只把结果**带回来**，由调用方写。
#[derive(Debug)]
pub enum Step {
    /// 验证码已发出，等用户输入
    OtpSent {
        verification_id: String,
        email: String,
        is_existing: bool,
    },
    /// 登录成功
    LoggedIn { token: String, email: Option<String> },
}

/// 【前置邮箱校验】—— 把服务端的校验规则**原样**搬到本地，提前拦下来。
///
/// 服务端（`.cloud/auth/v1/verification`）对一个不合法邮箱的回应是：
/// ```
/// {"code":"INVALID_ARGUMENT","error":"invalid_argument","error_code":3,
///  "error_description":"invalid SendVerificationCodeRequest.Email: value does not
///   match regex pattern \"^$|^[.0-9A-Za-z_-]{1,40}@[.0-9..."}
/// ```
/// 这段 protobuf 报文直接糊进对话框，用户只会看到"登录失败"。
///
/// 实测出来的规则（2026-10-11，逐个试过）：
/// | 输入 | 结果 |
/// |---|---|
/// | `a@qq.com` | ✅ |
/// | `a.b_c-d@qq.com` | ✅ |
/// | `a+b@qq.com` | ❌ **不允许 `+`** —— 而这是最常见的别名写法 |
/// | `测试@qq.com` | ❌ 不准中文 |
/// | `a@qq` | ❌ 域名必须带点 |
///
/// ⚠ 这里**只用 ASCII 判断**，不引正则 —— 要的是能指出"哪个字符不对"的提示，
///   而不是又一个"不匹配"。
pub fn validate_email(email: &str) -> std::result::Result<(), String> {
    let e = email.trim();
    if e.is_empty() {
        return Err("邮箱不能为空".into());
    }
    let Some((local, domain)) = e.split_once('@') else {
        return Err(if e.contains('＠') {
            "你把 @ 打成了全角「＠」—— 换成半角 @".into()
        } else {
            "要写成 name@example.com 这种形式".into()
        });
    };
    if local.is_empty() || local.chars().count() > 40 {
        return Err("@ 前面要是 1–40 个字符".into());
    }
    if let Some(c) = local
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "@ 前面不能有「{c}」—— 只允许英文、数字、点、下划线、减号。\
             ⚠ 不支持加号别名（name+tag@…），请用主邮箱地址"
        ));
    }
    if domain.is_empty() || !domain.contains('.') {
        return Err("域名部分要带点，例如 qq.com".into());
    }
    if let Some(c) = domain
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    {
        return Err(format!("域名里不能有「{c}」"));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err("域名后缀要至少 2 个英文字母，例如 .com".into());
    }
    Ok(())
}

/// 把**全角字符**折成半角，并去掉零宽字符，再去掉首尾空白 —— 然后才是要发出去的串。
///
/// 【为什么必须做这一步】
///   Phira 的输入框是自绘的，**不接管输入法的全角/半角状态**。
///   中文用户按 Shift+空格 切到全角之后，打出来的会是一串全角形近字：
///     期望 `pyzpansiyuan123456@qq.com`
///     实际 `ｐｙｚｐａｎｓｉｙｕａｎ１２３４５６＠ｑｑ．ｃｏｍ`
///   两者在字体里**看着几乎一样**，但正则一律不认，而服务端只会回一句
///   `value does not match regex pattern` —— 完全看不出问题在哪。
///
///   这里顺手也把 U+3000（全角空格）和零宽字符（U+200B/200C/200D/FEFF）去掉，
///   它们是复制粘贴时的常见污染。
pub fn normalize_email(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(*c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}'))
        .map(|c| {
            let u = c as u32;
            if (0xFF01..=0xFF5E).contains(&u) {
                // 全角 ASCII 区块 → 半角（＠→@，．→.，０→0，ａ→a …）
                char::from_u32(u - 0xFEE0).unwrap_or(c)
            } else if u == 0x3000 {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 发起第一步：发验证码。
pub async fn start_login(email: String) -> Result<Step> {
    let r = send_otp(&email).await?;
    if r.verification_id.is_empty() {
        bail!("服务端没有返回 verification_id");
    }
    Ok(Step::OtpSent {
        verification_id: r.verification_id,
        email,
        is_existing: r.is_user,
    })
}

/// 第二步：验证码换 token。
///
/// 新用户会走 `signup`，而它**要求密码**。客户端这里先不带密码
/// （`password: None` → 空串）—— 空密码账号以后不能用密码登录，
/// 但用验证码仍然能登，对我们是够的。
pub async fn finish_login(
    verification_id: String,
    code: String,
    email: String,
    is_existing: bool,
) -> Result<Step> {
    let s = verify_otp(&verification_id, &code, &email, is_existing, None).await?;
    if s.access_token.is_empty() {
        bail!("服务端没有返回 access_token");
    }
    Ok(Step::LoggedIn {
        token: s.access_token,
        email: s.email.or(Some(email)),
    })
}

// ══════════════════════════════════════════════════════════════
// 成绩上报
// ══════════════════════════════════════════════════════════════

/// 把 prpr 组好的载荷补上 `client_ver` 与 `chart_digest`，POST 到 `/api/hm/play`。
///
/// `chart_digest` 是 **sha256(谱面文件字节)，hex** —— 与 Phira 官方
/// `/chart/{id}/verify-cksum` 同一个算法（`song.rs:2307` 已有现成写法）。
/// 服务端据此判断「谱面有没有被改过」，digest 不一致的数据永不与其他数据混合。
///
/// `payload_json` 是 prpr 序列化的 `HuameiPayload`（`client_ver` 空、
/// `chart_digest` 为 null）。这里重新序列化成上传体，不原地改 JSON。
pub async fn upload_play(payload_json: &[u8], chart_digest: Option<String>) -> Result<UploadResp> {
    let Some(token) = current_token() else {
        bail!("未登录 huamei 账号");
    };
    let p: HuameiPayload =
        serde_json::from_slice(payload_json).context("prpr 给的对局载荷解析失败")?;

    let body = UploadBody {
        client_ver: HUAMEI_CLIENT_VER,
        judge_ver: p.judge_ver,
        chart_id: p.chart_id,
        chart_level: &p.chart_level,
        chart_digest: chart_digest.as_deref(),
        chart_notes: p.chart_notes,
        score: p.score,
        accuracy: p.accuracy,
        max_combo: p.max_combo,
        counts: p.counts,
        speed: p.speed,
        mods: p.mods,
        keyboard: p.keyboard,
        played_at: p.played_at,
        judge: &p.judge,
    };
    debug!(
        "huamei: 上报谱面 {} [{}] {} 分 {} 个音符",
        body.chart_id,
        body.chart_level,
        body.score,
        body.chart_notes
    );
    post_json::<_, UploadResp>(&format!("{}/play", hm_base()), &body, Some(&token)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户实测卡住的那一个：服务端接受了它，所以它必须通过本地校验。
    #[test]
    fn user_email_passes() {
        assert!(validate_email("pyzpansiyuan123456@qq.com").is_ok());
        assert!(validate_email("kanqiao86413@163.com").is_ok());
        assert!(validate_email("a.b_c-d@qq.com").is_ok());
    }

    /// 实测被服务端拒绝的（见 validate_email 的表格）
    #[test]
    fn known_bad_emails_rejected() {
        assert!(validate_email("a+b@qq.com").is_err(), "+ 别名服务端不收");
        assert!(validate_email("测试@qq.com").is_err());
        assert!(validate_email("a@qq").is_err());
        assert!(validate_email("a b@qq.com").is_err());
        assert!(validate_email("").is_err());
        // 提示要指出具体是哪个字符
        let e = validate_email("a+b@qq.com").unwrap_err();
        assert!(e.contains('+'), "提示里要出现那个字符：{e}");
    }

    /// 全角折算 —— 输入法切全角是这个 bug 最可能的成因
    #[test]
    fn fullwidth_is_folded() {
        let full = "ｐｙｚｐａｎｓｉｙｕａｎ１２３４５６＠ｑｑ．ｃｏｍ";
        assert_eq!(normalize_email(full), "pyzpansiyuan123456@qq.com");
        assert!(validate_email(&normalize_email(full)).is_ok());
        // 全角空格 / 零宽字符也要清掉
        assert_eq!(normalize_email("  a@qq.com\u{200B} "), "a@qq.com");
        assert_eq!(normalize_email("a@qq.com\u{3000}"), "a@qq.com");
        // 半角原样不动
        assert_eq!(normalize_email("a@qq.com"), "a@qq.com");
    }

    #[test]
    fn error_is_readable() {
        let raw = r#"{"code":"INVALID_ARGUMENT","error":"invalid_argument","error_code":3,"error_description":"invalid X.Email: bad"}"#;
        assert_eq!(readable_error(raw), "invalid X.Email: bad");
        assert_eq!(readable_error("plain text"), "plain text");
    }
}
