//! 【话梅】谱面定数客户端 —— 从 PHM 开放平台取定数。
//!
//! ## 接口（来自 phm-web 的 docs/API-V1.md）
//!
//! ```text
//! GET https://phm.app.workbuddy.host/api/v1/charts/{chart_id}
//! → {
//!     "chart_id": 22681,
//!     "level": "IN.16",
//!     "difficulty": 16.6,      ← 我们用的是这个
//!     "ref_const": 15.6,        ← 社区共识标度（另一套口径）
//!     "ref_official": 16.1,     ← 官谱标度
//!     "cached": true
//!   }
//! ```
//!
//! 匿名额度 100 次/小时，带 Key 1000 次/小时。我们**匿名调用 + 本地长缓存**：
//! 定数几乎不会变（官方已停更，PHM 的引擎版本才决定它），
//! 所以一个 chart_id 只需要取一次，缓存在本地永久有效。
//!
//! ## 为什么要有降级
//!
//! 玩家可能离线、可能在飞机上、可能站点临时 403。
//! 拿不到定数时**不能阻断任何东西** —— 用谱面自带的难度标签
//! （如 "IN.16" → 16.0）兜底，并在 UI 上标明是估算值。

use serde::{Deserialize, Serialize};

/// 定数来源，决定 UI 上要不要标注「估算」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DifficultySource {
    /// 来自 PHM 开放平台（权威）
    Phm,
    /// 从难度标签里抠出来的整数（估算，如 "IN.16" → 16.0）
    FromLabel,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Difficulty {
    pub value: f32,
    pub source: DifficultySource,
}

impl Difficulty {
    pub fn is_estimated(&self) -> bool {
        matches!(self.source, DifficultySource::FromLabel)
    }
}

/// PHM 返回体里我们关心的字段。
#[derive(Deserialize)]
struct ChartResp {
    #[serde(default)]
    difficulty: Option<f32>,
    #[serde(default)]
    level: Option<String>,
}

/// 从难度标签估算定数：取标签里的数字部分。
///
/// `"IN.16"` → 16.0、`"AT 15"` → 15.0、`"15.3"` → 15.3。
/// 拿不到数字就返回 None —— 宁可没有，也不要编一个。
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

/// 拉一个谱面的定数。失败时返回 `Err`，由调用方决定是否降级。
pub async fn fetch_difficulty(chart_id: i32) -> anyhow::Result<Difficulty> {
    let url = format!(
        "{}/api/v1/charts/{}",
        crate::client::HUAMEI_SITE.trim_end_matches('/'),
        chart_id
    );
    let resp = crate::client::basic_client_builder()
        .build()?
        .get(crate::client::with_sharecode(&url))
        .header("accept", "application/json")
        .send()
        .await?
        .error_for_status()?
        .json::<ChartResp>()
        .await?;

    if let Some(d) = resp.difficulty.filter(|d| d.is_finite() && *d > 0.) {
        return Ok(Difficulty {
            value: d,
            source: DifficultySource::Phm,
        });
    }
    // 服务端有谱面但还没分析过（analyzed:false / difficulty 缺失）——
    // 退回标签估算，别把它当成错误。
    let label = resp.level.unwrap_or_default();
    Ok(Difficulty {
        value: difficulty_from_label(&label).unwrap_or(1.),
        source: DifficultySource::FromLabel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_parsing_common_formats() {
        assert_eq!(difficulty_from_label("IN.16"), Some(16.0));
        assert_eq!(difficulty_from_label("AT 15"), Some(15.0));
        assert_eq!(difficulty_from_label("15.3"), Some(15.3));
        assert_eq!(difficulty_from_label("HD.8"), Some(8.0));
    }

    #[test]
    fn label_parsing_rejects_garbage() {
        assert_eq!(difficulty_from_label(""), None);
        assert_eq!(difficulty_from_label("IN"), None);
        assert_eq!(difficulty_from_label("?!"), None);
    }

    #[test]
    fn estimate_flag() {
        let d = Difficulty { value: 16.0, source: DifficultySource::FromLabel };
        assert!(d.is_estimated());
        let d = Difficulty { value: 16.6, source: DifficultySource::Phm };
        assert!(!d.is_estimated());
    }
}
