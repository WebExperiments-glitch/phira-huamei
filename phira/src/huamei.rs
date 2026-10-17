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
    client::{basic_client_builder, HUAMEI_CLIENT_VER, HUAMEI_PUBLISHABLE_KEY, HUAMEI_REFERER, HUAMEI_SITE},
    get_data,
};

/// 云鉴权要求的应用标识头。**名字必须是这个**。
const KEY_HEADER: &str = "x-wb-webapp-access-key";

// 基址从 `HUAMEI_SITE` 派生（`const` 里没法拼接运行时才知道的串，用函数）。
// 全部走我们自己的 server.mjs：`/api/hm/*`。
// ⚠ 这里**曾经**还有第二个基址 `/.cloud/auth/v1`（平台云认证）。换成自建账号后
//   整条路都走 `/api/hm/*`，那个基址和它专用的 me()/Session/OtpStarted 一起删了。
fn hm_base() -> String {
    format!("{HUAMEI_SITE}/api/hm")
}

#[derive(Debug, Deserialize)]
pub struct UploadResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub play_id: Option<i64>,
    /// 服务端重算 score/accuracy 后的**一致性告警**（非空表示有差异）。
    ///
    /// ⚠ 这不是错误 —— 成绩**已经落库**了。服务端刻意不为它拒收，
    ///   因为「算出来不等」未必是造假（也可能我们对计分的理解有偏差），
    ///   而拒收真实成绩比放行可疑数据糟得多。见契约 §8。
    ///   留在这里是为了把差异带进 `data/huamei-login.log`，好定位。
    #[serde(default)]
    pub warn: Vec<String>,
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
    /* ⚠ 必须带上 Referer / Origin。
       `phm-web/lib/cloud.mjs` 顶部写得很明确：
       「认证头是 x-wb-webapp-access-key（= publishableKey），**身份由 referer 判定**」。
       浏览器里跑 SDK 时这两个头是自动带的；原生客户端不带，
       服务端就可能把我们当成**另一个 client** —— 那正是
       `the session is invalid, expired or issued for another client` 的措辞。
       （实测单看某个端点用假 token 试不出来，但没理由不发。） */
    headers.insert(
        "referer",
        HeaderValue::from_static(HUAMEI_REFERER),
    );
    headers.insert(
        "origin",
        HeaderValue::from_static(HUAMEI_SITE),
    );
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

/// 往调试日志追加一行（同样的落盘策略，见 `log_login_debug`）。
pub fn log_debug_line(line: &str) {
    use std::io::Write;
    let Ok(root) = crate::dir::root() else { return };
    let path = std::path::Path::new(&root).join("huamei-login.log");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(f, "{line}");
}

async fn post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    step: &str,
    url: &str,
    body: &T,
    token: Option<&str>,
) -> Result<R> {
    let mut req = bare_client().post(url).json(body);
    if let Some(t) = token {
        // ⚠ **`x-hm-token` 才是主用的那个** —— 实测托管边缘会把入站的
        //   `Authorization` 头吃掉/改掉（我们的服务端收到后解析出来根本不是一个
        //   JWT），所以两个都发，服务端优先读自定义头。见 server.mjs 的注释。
        req = req.header("x-hm-token", t.to_owned());
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = req.send().await.with_context(|| "网络请求失败（检查网络/代理）".to_string())?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    /* ⚠ 每一步都落盘。
       起因：「the session is invalid, expired or issued for another client（401）」
       这条报错在四个候选步骤里我**逐个用假值试都试不出来** ——
       没有「哪一步、什么响应体」的记录就无从定位。
       所以：成功记状态码，失败连响应体一起记。 */
    if status.is_success() {
        /* 成功也带上响应体（截断）—— 「上报成功但没落库」这类问题只能靠它定位，
           例如响应里的 `play_id` / `verified` / `warn` 都在这儿。 */
        log_debug_line(&format!(
            "[step] {step} → HTTP {status} ok body={}",
            text.chars().take(200).collect::<String>()
        ));
    } else {
        log_debug_line(&format!(
            "[step] {step} → HTTP {status} FAIL body={}",
            text.chars().take(400).collect::<String>()
        ));
    }

    if !status.is_success() {
        // 错误里带上**是哪一步**，别只甩一句服务端的英文
        bail!("{step} 失败（HTTP {status}）：{}", readable_error(&text));
    }
    serde_json::from_str::<R>(&text).with_context(|| format!("{step}：服务端返回的不是预期 JSON"))
}

// ══════════════════════════════════════════════════════════════
// 登录
// ══════════════════════════════════════════════════════════════

/// 第一步：发邮箱验证码。返回 `verification_id`（下一步要用）。
///
/// 对应 SDK 的 `auth.sendOtp({ email })`。
/// 自建账号：注册 / 登录。
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
    /* 服务端还会回 `sub`（账号 id）。**故意不接** —— 它是 token 里的一个声明，
       客户端拿它没有任何用处（上报时服务端自己从 token 取）。多存一份就多一处
       可能与 token 不一致的地方，所以只在需要时从服务端拿。 */
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
    code: Option<&str>,
) -> std::result::Result<String, (String, Option<String>)> {
    let mut body = serde_json::json!({ "email": email, "password": password });
    /* 验证码只在注册时带。**登录永远不带** —— 产品定的是「注册要码、登录只要密码」，
       登录侧靠 per-IP + per-邮箱 三重限流兜着。 */
    if let Some(c) = code {
        body["code"] = serde_json::Value::String(c.to_owned());
    }
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
        /* ── 验证码：每一条都要能直接指导下一步，别只说"失败" ── */
        "code_required" => "这次注册需要邮箱验证码，但没有带上 —— 请重新走一遍注册".to_string(),
        "code_missing" => "没找到这个邮箱的验证码，请点「注册」重新发一封".to_string(),
        "code_wrong" => "验证码不对，请照着邮件里最新的那封填".to_string(),
        "code_invalid" => "验证码格式不对（应该是邮件里那串数字）".to_string(),
        "code_expired" => "验证码过期了（10 分钟有效），请重新发一封".to_string(),
        "code_used" => "这个验证码已经用过了，请重新发一封".to_string(),
        "code_too_many" => "这个验证码试错太多次，已作废 —— 请重新发一封".to_string(),
        "send_failed" => "验证码没发出去（服务端的邮件通道出问题了），请稍后再试".to_string(),
        "verify_failed" => "验证服务暂时不可用，请稍后再试".to_string(),
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

/// 注册：邮箱 + 密码 + **邮箱验证码**。
///
/// 【为什么必须验证码】
///   不要码 = 谁都能拿别人的邮箱批量注册（白帽报告第 4 条）。服务端
///   `REQUIRE_EMAIL_CODE` 打开后缺码会回 `code_required`，这里翻译成
///   「验证码没填/过期了，重新发一次」。
pub async fn register(email: &str, password: &str, code: &str) -> Result<Step> {
    let token = account_call("注册", "register", email, password, Some(code))
        .await
        .map_err(|(err, detail)| account_error("注册", &err, detail.as_deref()))?;
    Ok(Step::LoggedIn {
        token,
        email: Some(email.to_owned()),
    })
}

/// 请求一封注册验证码。
///
/// ⚠ 服务端对**已注册**的邮箱会回 `email_taken` —— 这不是错误，是提示：
///   直接告诉他「这个邮箱注册过了，直接输密码登录」，比让他输完码再被拒好。
pub async fn send_code(email: &str, purpose: &str) -> Result<()> {
    let body = serde_json::json!({ "email": email, "purpose": purpose });
    let resp = bare_client()
        .post(format!("{}/send-code", hm_base()))
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("连不上服务器，验证码没发出去：{e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    log_debug_line(&format!(
        "[step] send-code → HTTP {status} ok body={}",
        text.chars().take(200).collect::<String>()
    ));
    let r: AccountResp = serde_json::from_str(&text)
        .map_err(|_| anyhow::anyhow!("服务端返回了看不懂的内容，验证码可能没发出去"))?;
    if !status.is_success() || !r.ok {
        let code = r.error.unwrap_or_else(|| format!("http_{}", status.as_u16()));
        /* 这里借 account_error 的翻译，但**去掉「注册」前缀** —— 用户看到的
           应该是「验证码没发出去」，不是「注册失败」。 */
        return Err(account_error("发送验证码", &code, r.detail.as_deref()));
    }
    Ok(())
}

/// 登录：邮箱 + 密码。邮箱由客户端记住，用户只输密码。
pub async fn login(email: &str, password: &str) -> Result<Step> {
    let token = account_call("登录", "login", email, password, None)
        .await
        .map_err(|(code, detail)| account_error("登录", &code, detail.as_deref()))?;
    Ok(Step::LoggedIn {
        token,
        email: Some(email.to_owned()),
    })
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
    /// 注册 / 登录成功，拿到 token
    LoggedIn { token: String, email: Option<String> },
}

/// 【前置邮箱校验】—— 把服务端的校验规则**原样**搬到本地，提前拦下来。
///
/// 【原来的写法】这里贴的是平台云认证对一个不合法邮箱的回应。换成自建账号后服务端换了，但「提前拦下来、指出哪个字符不对」这件事不变，
/// ```
/// {"code":"INVALID_ARGUMENT","error":"invalid_argument","error_code":3,
///  "error_description":"invalid SendVerificationCodeRequest.Email: value does not
///   match regex pattern \"^$|^[.0-9A-Za-z_-]{1,40}@[.0-9..."}
/// ```
/// 规则仍以 `phm-web/lib/hm-accounts.mjs` 的 validateEmail 为准。
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
    /* ⚠ 每个错误都带上**实际拿到的串**。
       这条规则是血换来的：2026-10-11 用户报「显示 @ 前面要是 1–40 个字符」，
       但截图里输入的是 `pyzpansiyuan123456@qq. com`（18 个字符的 local，
       含一个空格）—— 和那条消息**对不上**，因为消息没带证据，
       谁也说不清那次到底传了什么。现在不允许再出现这种无从对证的情况。 */
    let shown = if e.chars().count() > 60 {
        format!("{}…", e.chars().take(60).collect::<String>())
    } else {
        e.to_owned()
    };
    let Some((local, domain)) = e.split_once('@') else {
        return Err(if e.contains('＠') {
            format!("「{shown}」里的 @ 是全角「＠」—— 换成半角 @")
        } else {
            format!("「{shown}」里没有 @ —— 要写成 name@example.com")
        });
    };
    if local.is_empty() {
        return Err(format!("「{shown}」的 @ 前面是空的"));
    }
    let n_local = local.chars().count();
    if n_local > 40 {
        return Err(format!(
            "@ 前面有 {n_local} 个字符（最多 40）。\
             实际收到的是：{}",
            debug_repr(e, 90)
        ));
    }
    if domain.contains('@') {
        return Err(format!("「{shown}」里有不止一个 @"));
    }
    /* ⚠ 允许的字符集**照抄服务端**（`lib/hm-accounts.mjs` 的 validateEmail）：
       英文、数字、以及 . ! # $ % & ' * + / = ? ^ _ ` { | } ~ -
       以前禁 `+` 是**平台**的限制（它的正则不收），现在我们自己存账号，
       没有理由继承。两边规则必须一致，否则会出现「本地说合法、服务端说非法」。 */
    if let Some(c) = local
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~-".contains(*c)))
    {
        return Err(format!(
            "「{shown}」：@ 前面不能有「{c}」—— 只允许英文、数字和 . ! # $ % & ' * + / = ? ^ _ ` {{ | }} ~ -"
        ));
    }
    if domain.is_empty() || !domain.contains('.') {
        return Err(format!("「{shown}」：域名部分要带点，例如 qq.com"));
    }
    if let Some(c) = domain
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    {
        return Err(format!("「{shown}」：域名里不能有「{c}」"));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(format!("「{shown}」：域名后缀要至少 2 个英文字母，例如 .com"));
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
/// 从对话框回传的整段文本里**抠出邮箱** —— 去掉前面的进程噪声。
///
/// ══════════════════════════════════════════════════════════════
/// 【为什么会有噪声 —— 2026-10-11 实测（`data/huamei-login.log`）】
/// ══════════════════════════════════════════════════════════════
/// ```text
/// raw = "[WARNING] Ignore caches that are heterogeneous\r\npyzpansiyuan123456@qq.com"
/// ```
/// 那个 WinForms 输入框（`inputbox` crate）读的是**子进程 stdout 全文**，
/// 而子进程（系统脚本宿主）在初始化 WinForms/GDI+ 时会把它**自己**的
/// 显卡驱动/字体缓存告警也写进 stdout —— 于是它和文本框内容粘在一起。
///
/// 这就是「只输了 18 个字符却报 59 个」的**第二层**原因（第一层是 UTF-16）。
///
/// 【抠取规则：取「最后一行里含 `@` 的那一行」】
///   脚本是**最后**才写文本框内容的 → 真值在末尾；
///   噪声（日志/告警）都是**整行**的 → 用换行切就够。
///
/// ⚠ **不能改用「从 @ 向左右扩到空白边界」** —— 那条规则会和另一个真实需求冲突：
///   用户在 `qq. com` 中间多打一个空格时，那个空格也是"边界"，
///   会把 `com` 直接切掉（实测踩过：切出来是 `…@qq.`）。
///   按行切 + 之后统一删空白，两个场景才都对。
///
/// ⚠ 找不到带 `@` 的行就退回整串（清洗后），交给 `validate_email` 报错 ——
///   **绝不猜**，宁可不发也不发一个错的地址。
pub fn extract_email(text: &str) -> String {
    // 逐行清洗：删空白、NUL/U+FFFD、零宽字符
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .filter(|c| !matches!(*c, '\0' | '\u{FFFD}'))
            .filter(|c| !matches!(*c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}'))
            .collect()
    };
    // 从后往前找第一个「清洗后仍然非空且含 @」的行
    if let Some(line) = text
        .lines()
        .rev()
        .map(clean)
        .find(|l| !l.is_empty() && l.contains('@'))
    {
        return line;
    }
    clean(text)
}

/// 把输入框吐出来的东西洗成一个正常的邮箱串。
///
/// ══════════════════════════════════════════════════════════════
/// 【这个输入框有两层坑，都实测到了】
/// ══════════════════════════════════════════════════════════════
/// **第一层：UTF-16 被当成 UTF-8 读。**
/// Windows 上输入框是 `inputbox` crate 跑系统脚本宿主 + WinForms，
/// Rust 侧取的是 `output.stdout` 的全部内容：
/// ```ignore
/// Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
/// ```
/// 而宿主**按 UTF-16 LE（带 BOM）写出**，实测原始字节：
/// ```text
/// ff fe | 70 00 79 00 72 00 | 0d 00 0a 00
///  BOM  |  p \0  y \0  r \0 |  \r \0 \n \0
/// ```
/// 按 UTF-8 读 → `\u{FFFD}\u{FFFD}p\0y\0r\0…`
/// —— **每个字符后面多一个 NUL，长度接近翻倍**。
///
/// **第二层：子进程自己的告警粘在文本前面**（见 `extract_email`）。
///
/// 【顺序不能乱】
///   1. 全角折半角（输入法切全角是真踩过的）
///   2. 逐行清洗并抠出邮箱（`extract_email`）—— 顺带把空白/NUL/FFFD/零宽全清掉
pub fn normalize_email(raw: &str) -> String {
    let folded: String = raw
        .chars()
        .map(|c| {
            let u = c as u32;
            if (0xFF01..=0xFF5E).contains(&u) {
                // 全角 ASCII 区块 → 半角（＠→@，．→.，０→0，ａ→a …）
                char::from_u32(u - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect();
    extract_email(&folded)
}

/// 诊断用的字符串表示：用 `{:?}` 转义，**不可见字符会显形**。
///
/// 【为什么需要它】
///   2026-10-11 用户报「@ 前面有 59 个字符」，可他明明只输了 18 个。
///   输入框在 Windows 上是跑 PowerShell 脚本弹原生窗，而 Rust 侧取的是
///   **`output.stdout` 的全部内容**当作返回值（`inputbox` 的 `wait_child`）——
///   只要 PowerShell 往 stdout 多写任何东西，就会混进"邮箱"里。
///
///   而 `{:?}` 会把 `\r` `\n` `\t` NUL 之类**原样显示成转义序列**，
///   不然它们在对话框里根本看不见，只能靠数长度猜。
pub fn debug_repr(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars().take(max) {
        match c {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) == 0xFEFF => out.push_str("\\uFEFF"),
            c => out.push(c),
        }
    }
    if s.chars().count() > max {
        out.push('…');
    }
    format!("「{out}」（{} 字符，{} 字节）", s.chars().count(), s.len())
}

/// 把一次登录输入的原貌追加到 `data/huamei-login.log`。
///
/// 【为什么落盘】
///   「59 个字符」这种问题**光看截图查不出来**（不可见字符不显示，
///   而且用户没法把奇怪字符复制给我）。写到文件里我就能直接读原始字节。
///   ⚠ 只在**校验失败**时写，且写完就用 —— 不做长期日志，避免把邮箱留在盘上。
pub fn log_login_debug(tag: &str, raw: &str, normalized: &str) {
    use std::io::Write;
    let Ok(root) = crate::dir::root() else { return };
    let path = std::path::Path::new(&root).join("huamei-login.log");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(
        f,
        "[{tag}] raw={} normalized={}",
        debug_repr(raw, 300),
        debug_repr(normalized, 300)
    );
}

/// 发起第一步：发验证码。
/// 密码格式校验（本地先拦一道，别什么都往服务端送）。
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
    post_json::<_, UploadResp>("上报成绩", &format!("{}/play", hm_base()), &body, Some(&token))
        .await
        .map(|r| {
            /* 有差异就落盘 —— 这是**唯一**能看到服务端复算结果的窗口。
               正常情况（客户端与服务端同一套公式）这里永远为空。 */
            if !r.warn.is_empty() {
                log_debug_line(&format!("[warn] 上报成绩 服务端一致性告警：{}", r.warn.join(" | ")));
            }
            r
        })
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

    /// 实测被服务端拒绝的（见 validate_email 的文档表格）
    #[test]
    fn known_bad_emails_rejected() {
        assert!(validate_email("a+b@qq.com").is_err(), "+ 别名服务端不收");
        assert!(validate_email("测试@qq.com").is_err());
        assert!(validate_email("a@qq").is_err());
        assert!(validate_email("").is_err());
        assert!(validate_email("a@@qq.com").is_err(), "两个 @");
        // 提示要指出具体是哪个字符
        let e = validate_email("a+b@qq.com").unwrap_err();
        assert!(e.contains('+'), "提示里要出现那个字符：{e}");
    }

    /// ⚠ **实测出来的真凶**：输入框把文本按 **UTF-16 LE** 吐出来，
    /// 而 Rust 侧按 UTF-8 读 → 每个字符后面多一个 NUL，长度翻倍，
    /// 头两个字节（BOM `ff fe`）变成两个 `U+FFFD`。
    ///
    /// 这一条用**逐字节复现的真实污染串**来锁：
    /// `ff fe 70 00 79 00 ... 0d 00 0a 00` 按 UTF-8 lossy 读出来是什么。
    #[test]
    fn utf16_read_as_utf8_is_repaired() {
        // 逐字节复现：BOM(ff fe) 按 UTF-8 lossy 读 → 两个 U+FFFD；
        // 随后每个 ASCII 字符后面跟一个 NUL（UTF-16LE 的高位字节）。
        let mut polluted = String::from("\u{FFFD}\u{FFFD}");
        for c in "pyrpansiyuan123456@qq.com".chars() {
            polluted.push(c);
            polluted.push('\0');
        }
        polluted.push_str("\r\0\n\0");

        assert_eq!(normalize_email(&polluted), "pyrpansiyuan123456@qq.com");
        assert!(validate_email(&normalize_email(&polluted)).is_ok());
        // 也能解释「长度翻倍」：清洗前字符数接近两倍
        assert!(polluted.chars().count() > 45);
    }

    /// ⚠⚠ **实测抓到的第二层污染**：子进程自己的告警粘在文本前面。
    /// 这一条用 `data/huamei-login.log` 里那行**原样**的真实数据做用例。
    #[test]
    fn extracts_email_from_process_noise() {
        let raw = "[WARNING] Ignore caches that are heterogeneous\r\npyzpansiyuan123456@qq.com";
        assert_eq!(normalize_email(raw), "pyzpansiyuan123456@qq.com");
        assert!(validate_email(&normalize_email(raw)).is_ok());

        // 多行噪声也要能切
        assert_eq!(normalize_email("line one\nline two\n   \n  a@qq.com  \n"), "a@qq.com");
        // ⚠ 两个需求会打架：噪声按行切，但**地址中间的空格要保留不住** ——
        //   `qq. com` 里的空格不是行边界，所以整行清洗后仍然修得回来。
        assert_eq!(normalize_email("noise line\r\npyr@qq. com"), "pyr@qq.com");
    }

    /// 抠不出邮箱时**绝不猜** —— 退回整串，交给校验报错
    #[test]
    fn never_guesses_when_unparseable() {
        assert_eq!(normalize_email("no at sign here"), "noatsignhere");
        assert!(validate_email(&normalize_email("no at sign here")).is_err());
        assert_eq!(normalize_email(""), "");
        assert!(validate_email("").is_err());
    }

    /// 每一种污染单独拎出来也要能修
    #[test]
    fn all_pollution_is_scrubbed() {
        // NUL（UTF-16 残留）
        assert_eq!(normalize_email("a\0@\0q\0q\0.\0c\0o\0m\0"), "a@qq.com");
        // U+FFFD（无效字节的 lossy 替换符）
        assert_eq!(normalize_email("\u{FFFD}\u{FFFD}a@qq.com"), "a@qq.com");
        // 三者叠加
        assert_eq!(normalize_email("\u{FFFD}a\0@\0q\0q\0.\u{FFFD}c\0o\0m\0"), "a@qq.com");
    }

    /// 用户截图里那个串：`qq.` 和 `com` 之间**多了一个空格**。
    /// 空白在邮箱里没有任何合法位置，必须被自动删掉（不是只 trim 首尾）。
    #[test]
    fn stray_space_is_removed() {
        assert_eq!(normalize_email("pyzpansiyuan123456@qq. com"), "pyzpansiyuan123456@qq.com");
        assert!(validate_email(&normalize_email("pyzpansiyuan123456@qq. com")).is_ok());
        // 首尾空白、中间空白、全角空格、制表符、换行 —— 一律清掉
        assert_eq!(normalize_email("  a @ qq . com \n"), "a@qq.com");
        assert_eq!(normalize_email("a@qq.com\t"), "a@qq.com");
        assert_eq!(normalize_email("a@qq.com\u{3000}"), "a@qq.com");
    }

    /// 每条错误都必须带**实际拿到的串** —— 否则出了问题无从对证。
    /// （起因：用户报的那条消息和截图里的输入对不上，谁也说不清传了什么。）
    #[test]
    fn error_carries_the_actual_input() {
        let e = validate_email("pyrpansiyuan123456@qq. com").unwrap_err();
        assert!(e.contains("pyrpansiyuan123456"), "错误里要带实际串：{e}");
        assert!(e.contains(' '), "并且要指出是空格：{e}");
        let e2 = validate_email("aaaa").unwrap_err();
        assert!(e2.contains("aaaa"), "错误里要带实际串：{e2}");
    }

    /// 全角折算 —— 输入法切全角是这个 bug 最可能的成因
    #[test]
    fn fullwidth_is_folded() {
        let full = "ｐｙｚｐａｎｓｉｙｕａｎ１２３４５６＠ｑｑ．ｃｏｍ";
        assert_eq!(normalize_email(full), "pyzpansiyuan123456@qq.com");
        assert!(validate_email(&normalize_email(full)).is_ok());
        // 零宽字符也要清掉
        assert_eq!(normalize_email("  a@qq.com\u{200B} "), "a@qq.com");
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
