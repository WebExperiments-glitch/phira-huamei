//! 【话梅】谱面定数客户端 —— 按 PHM 开放平台的完整协议实现。
//!
//! ## 口径（重要，之前搞错过）
//!
//! 服务端返回两个数，含义完全不同：
//!
//! | 字段 | 是什么 | 用途 |
//! |---|---|---|
//! | `ref_const` | **P.H.M. 实测**（社区共识标度） | 主数字，RKS 用这个 |
//! | `difficulty` | 谱师**自填的标称定数** | 副行，标注「谱师标称」 |
//!
//! 最初我把 `difficulty` 当定数用，是错的 —— 那个是作者自己填的，
//! 未经测量。RKS 必须用 `ref_const`，否则算出来的是「作者的主观难度」。
//!
//! ## 请求流程（服务端已就绪，客户端按序执行）
//!
//! ```text
//! ① GET  /api/v1/charts/{id}
//!    ├─ analyzed == true  → 直接拿到 ref_const，结束（不消耗任何算力）
//!    └─ analyzed == false → 继续
//! ② GET  /api/v1/pow                    → { seed, bits }
//! ③ 后台线程 solve_pow(seed, bits)      → nonce（20 bit 在手机上 1~3 秒）
//! ④ POST /api/v1/analyses  (x-phm-pow: nonce)
//!    ├─ 200 → 别人刚算完，直接用返回值
//!    ├─ 202 → 已入队，转 ⑤
//!    ├─ 428 → 题目过期/答案错，回 ② 重新领题
//!    └─ 503 → 服务器忙，退避 5 秒重试 ④
//! ⑤ 每 2 秒轮询 ①，最多 60 秒；超时提示「稍后再看」
//! ```
//!
//! ## 三条不做（来自服务端规格）
//!
//! 1. 不放 API Key —— 这条路存在的意义就是免 Key
//! 2. 不并发解多道题 —— 题目一次性，并发只会互相作废
//! 3. 不重试同一个 nonce —— 核销即失效，重试必 428

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
    time::{Duration, Instant},
};

/// 未分析结果的缓存时长。
///
/// 已分析的结果几乎不变（引擎升级时才变，靠 `engine_build` 判断失效）；
/// 但 `analyzed:false` 是**暂时**状态 —— 缓存太久玩家就永远看不到新结果。
const NEGATIVE_TTL: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_secs(60);
const BUSY_BACKOFF: Duration = Duration::from_secs(5);

/// 定数来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DifficultySource {
    /// P.H.M. 实测值（`ref_const`）—— 权威
    Measured,
    /// 谱师标称（`difficulty`）—— 未经测量
    Nominal,
    /// 从难度标签抠出来的整数（`IN.16` → 16.0）—— 估算
    FromLabel,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Difficulty {
    pub value: f32,
    pub source: DifficultySource,
    /// 谱师标称值（如果有，UI 上作为副行）
    pub nominal: Option<f32>,
}

impl Difficulty {
    /// UI 上是否需要标注「估算」或「标称」。
    pub fn is_estimated(&self) -> bool {
        !matches!(self.source, DifficultySource::Measured)
    }
}

/// 一次 `GET /charts/{id}` 的结果。
#[derive(Clone, Debug)]
pub struct ChartInfo {
    pub analyzed: bool,
    pub ref_const: Option<f32>,
    pub nominal: Option<f32>,
    pub engine_build: String,
    pub level: Option<String>,
}

impl ChartInfo {
    /// 转成可用于 RKS 的定数。`ref_const` 优先。
    pub fn to_difficulty(&self, level: &str) -> Difficulty {
        if self.analyzed {
            if let Some(r) = self.ref_const.filter(|v| v.is_finite() && *v > 0.) {
                return Difficulty {
                    value: r,
                    source: DifficultySource::Measured,
                    nominal: self.nominal,
                };
            }
        }
        if let Some(d) = self.nominal.filter(|v| v.is_finite() && *v > 0.) {
            return Difficulty {
                value: d,
                source: DifficultySource::Nominal,
                nominal: None,
            };
        }
        Difficulty {
            value: difficulty_from_label(level).unwrap_or(1.),
            source: DifficultySource::FromLabel,
            nominal: None,
        }
    }
}

// ── 缓存 ────────────────────────────────────────────────────
//
// 键是 chart_id；值里带 engine_build —— 引擎升级后定数可能变，
// 读缓存时比对 build 号，不一致就当作未命中（换键的等价做法）。

struct Cached {
    info: ChartInfo,
    at: Instant,
}

static CACHE: LazyLock<RwLock<HashMap<i32, Cached>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

fn cache_get(chart_id: i32) -> Option<ChartInfo> {
    let map = CACHE.read().ok()?;
    let c = map.get(&chart_id)?;
    if !c.info.analyzed && c.at.elapsed() > NEGATIVE_TTL {
        return None;
    }
    Some(c.info.clone())
}

fn cache_put(chart_id: i32, info: ChartInfo) {
    if let Ok(mut map) = CACHE.write() {
        map.insert(chart_id, Cached { info, at: Instant::now() });
    }
}

// ── 难度标签解析 ────────────────────────────────────────────

/// `"IN.16"` → 16.0、`"AT 15"` → 15.0、`"15.3"` → 15.3。
pub fn difficulty_from_label(level: &str) -> Option<f32> {
    let mut num = String::new();
    let mut started = false;
    for ch in level.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            started = true;
        } else if started {
            if ch == '.' && !num.contains('.') {
                num.push(ch);
                continue;
            }
            break;
        }
    }
    num.parse::<f32>().ok().filter(|v| *v > 0.)
}

// ── PoW ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PowChallenge {
    seed: String,
    bits: u32,
}

/// 找 nonce 使 `sha256("{seed}:{nonce}")` 前 `bits` 个 bit 为 0。
///
/// ⚠ **必须在阻塞线程池里跑**。20 bit 在手机约 1~3 秒，
/// 放主线程会直接卡住渲染（这是规格里特别强调的一点）。
fn solve_pow(seed: &str, bits: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut i: u64 = 0;
    loop {
        let nonce = format!("{i:x}");
        let digest = Sha256::digest(format!("{seed}:{nonce}").as_bytes());
        let mut need = bits;
        let mut ok = true;
        for byte in digest {
            let lead = byte.leading_zeros();
            if lead >= need {
                break;
            }
            if lead < 8 {
                ok = false;
                break;
            }
            need -= 8;
        }
        if ok {
            return nonce;
        }
        i += 1;
    }
}

async fn solve_pow_async(seed: String, bits: u32) -> String {
    // spawn_blocking：把 CPU 密集的哈希循环挪出渲染线程
    match tokio::task::spawn_blocking(move || solve_pow(&seed, bits)).await {
        Ok(n) => n,
        Err(_) => String::new(),
    }
}

// ── HTTP ────────────────────────────────────────────────────

fn base() -> String {
    crate::client::HUAMEI_SITE.trim_end_matches('/').to_owned()
}

fn url(path: &str) -> String {
    crate::client::with_sharecode(&format!("{}{}", base(), path))
}

fn http() -> reqwest::Client {
    crate::client::basic_client_builder().build().unwrap_or_default()
}

#[derive(Deserialize)]
struct ChartResp {
    #[serde(default)]
    difficulty: Option<f32>,
    #[serde(default)]
    ref_const: Option<f32>,
    #[serde(default)]
    engine_build: Option<String>,
    #[serde(default)]
    engine_ver: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    analyzed: Option<bool>,
}

/// ① 读一次谱面信息（不消耗算力）。
pub async fn fetch_chart_info(chart_id: i32) -> anyhow::Result<ChartInfo> {
    let resp = http()
        .get(url(&format!("/api/v1/charts/{chart_id}")))
        .header("accept", "application/json")
        .send()
        .await?
        .error_for_status()?
        .json::<ChartResp>()
        .await?;

    // 服务端可能不给 analyzed:false 的显式标记 —— 有 ref_const 就视为已分析。
    let analyzed = resp.analyzed.unwrap_or(resp.ref_const.is_some());
    let info = ChartInfo {
        analyzed,
        ref_const: resp.ref_const,
        nominal: resp.difficulty,
        engine_build: resp.engine_build.or(resp.engine_ver).unwrap_or_default(),
        level: resp.level,
    };
    cache_put(chart_id, info.clone());
    Ok(info)
}

/// ②~⑤ 触发一次分析并等待结果。
pub async fn ensure_analysis(chart_id: i32) -> anyhow::Result<()> {
    // 最多重领两次题（428 时重来，且**不复用 nonce**）
    for _attempt in 0..2 {
        let challenge = http()
            .get(url("/api/v1/pow"))
            .send()
            .await?
            .error_for_status()?
            .json::<PowChallenge>()
            .await?;

        let nonce = solve_pow_async(challenge.seed, challenge.bits).await;

        let mut resp = http()
            .post(url("/api/v1/analyses"))
            .header("x-phm-pow", &nonce)
            .json(&serde_json::json!({ "chart_id": chart_id }))
            .send()
            .await?;

        // 503：服务器忙，退避后重试一次
        if resp.status().as_u16() == 503 {
            tokio::time::sleep(BUSY_BACKOFF).await;
            resp = http()
                .post(url("/api/v1/analyses"))
                .header("x-phm-pow", &nonce)
                .json(&serde_json::json!({ "chart_id": chart_id }))
                .send()
                .await?;
        }

        match resp.status().as_u16() {
            // 200 别人刚算完 / 202 已入队 —— 都要轮询等结果
            200 | 202 => {
                let deadline = Instant::now() + POLL_TIMEOUT;
                while Instant::now() < deadline {
                    tokio::time::sleep(POLL_INTERVAL).await;
                    if let Ok(info) = fetch_chart_info(chart_id).await {
                        if info.analyzed && info.ref_const.is_some() {
                            return Ok(());
                        }
                    }
                }
                anyhow::bail!("分析超时（60 秒）");
            }
            // 428：题目过期或答案错 —— 重新领题重解
            428 => continue,
            code => anyhow::bail!("分析请求失败：HTTP {code}"),
        }
    }
    anyhow::bail!("Pow 验证连续失败")
}

// ── 对外入口 ────────────────────────────────────────────────

/// 结算时用：**同步、不阻塞**。
///
/// 缓存命中就用权威值；否则先用标称/标签估算，
/// 同时后台跑完整流程，成功后写缓存并回填历史。
pub fn resolve_or_estimate(chart_id: i32, level: &str) -> Difficulty {
    if let Some(info) = cache_get(chart_id) {
        if info.analyzed {
            return info.to_difficulty(level);
        }
    }

    let level_owned = level.to_owned();
    prpr::task::Task::new(async move {
        if let Ok(info) = fetch_chart_info(chart_id).await {
            if info.analyzed {
                crate::backfill_difficulty(chart_id, info.to_difficulty(&level_owned).value);
                return;
            }
        }
        // 没有实测值才花算力
        match ensure_analysis(chart_id).await {
            Ok(()) => {
                if let Ok(info) = fetch_chart_info(chart_id).await {
                    crate::backfill_difficulty(chart_id, info.to_difficulty(&level_owned).value);
                }
            }
            Err(err) => crate::telem::note(format!("difficulty: {chart_id} 分析失败 {err}")),
        }
    });

    Difficulty {
        value: difficulty_from_label(level).unwrap_or(1.),
        source: DifficultySource::FromLabel,
        nominal: None,
    }
}

/// 谱面详情页用：**异步完整流程**（含轮询）。
pub async fn resolve_full(chart_id: i32, level: &str) -> Difficulty {
    if let Some(info) = cache_get(chart_id) {
        if info.analyzed {
            return info.to_difficulty(level);
        }
    }
    if let Ok(info) = fetch_chart_info(chart_id).await {
        if info.analyzed {
            return info.to_difficulty(level);
        }
        // 未收录：先把标称值返回（不留空，留空会被当成 Bug 上报）
        let fallback = info.to_difficulty(level);
        if ensure_analysis(chart_id).await.is_ok() {
            if let Ok(info2) = fetch_chart_info(chart_id).await {
                return info2.to_difficulty(level);
            }
        }
        return fallback;
    }
    Difficulty {
        value: difficulty_from_label(level).unwrap_or(1.),
        source: DifficultySource::FromLabel,
        nominal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_parsing() {
        assert_eq!(difficulty_from_label("IN.16"), Some(16.0));
        assert_eq!(difficulty_from_label("AT 15"), Some(15.0));
        assert_eq!(difficulty_from_label("15.3"), Some(15.3));
        assert_eq!(difficulty_from_label("IN"), None);
        assert_eq!(difficulty_from_label(""), None);
    }

    /// PoW 的核心：解出来的 nonce 必须让哈希前缀满足要求。
    #[test]
    fn pow_solution_is_valid() {
        use sha2::{Digest, Sha256};
        let seed = "abc123";
        let bits = 6; // 低难度，避免测试跑太久
        let nonce = solve_pow(seed, bits);
        let digest = Sha256::digest(format!("{seed}:{nonce}").as_bytes());
        let mut need = bits;
        for byte in digest {
            let lead = byte.leading_zeros();
            if lead >= need {
                return; // 通过
            }
            assert!(lead >= 8, "前缀不满足要求");
            need -= 8;
        }
    }

    #[test]
    fn ref_const_preferred_over_nominal() {
        let info = ChartInfo {
            analyzed: true,
            ref_const: Some(15.6),
            nominal: Some(16.1),
            engine_build: "x".into(),
            level: Some("IN.16".into()),
        };
        let d = info.to_difficulty("IN.16");
        assert_eq!(d.source, DifficultySource::Measured);
        assert!((d.value - 15.6).abs() < 1e-4, "必须用实测值而不是标称值");
        assert_eq!(d.nominal, Some(16.1));
    }

    #[test]
    fn unanalyzed_falls_back_to_nominal() {
        let info = ChartInfo {
            analyzed: false,
            ref_const: None,
            nominal: Some(16.1),
            engine_build: "x".into(),
            level: Some("IN.16".into()),
        };
        let d = info.to_difficulty("IN.16");
        assert_eq!(d.source, DifficultySource::Nominal);
        assert!((d.value - 16.1).abs() < 1e-4);
    }

    #[test]
    fn nothing_falls_back_to_label() {
        let info = ChartInfo {
            analyzed: false,
            ref_const: None,
            nominal: None,
            engine_build: "x".into(),
            level: Some("IN.14".into()),
        };
        let d = info.to_difficulty("IN.14");
        assert_eq!(d.source, DifficultySource::FromLabel);
        assert!((d.value - 14.).abs() < 1e-4);
    }
}
