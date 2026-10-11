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

async fn post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    url: &str,
    body: &T,
    token: Option<&str>,
) -> Result<R> {
    let mut req = bare_client().post(url).json(body);
    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = req.send().await.with_context(|| format!("POST {url} 失败"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("POST {url} → {status}: {}", text.chars().take(200).collect::<String>());
    }
    serde_json::from_str::<R>(&text)
        .with_context(|| format!("POST {url} 返回的不是预期 JSON：{}", text.chars().take(200).collect::<String>()))
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
