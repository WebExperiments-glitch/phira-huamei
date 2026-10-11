//! huamei 数据飞轮 —— 对局记录的结构与紧凑编码。
//!
//! 【为什么放在 prpr 而不是 phira】
//!   逐音符判定只存在于 `Judge` 里，而 `Judge` 属于 prpr。prpr 又不能反向依赖
//!   phira（网络层在那边）。所以**数据**在 prpr 组装，**发送**在 phira 完成：
//!   prpr 把 `HuameiPayload` 序列化成 JSON 字节交给既有的 `UploadFn`，
//!   phira 侧补上谱面摘要再 POST。
//!
//! 【零改动复用上游管道】
//!   上游已经有一条「结算 → EndingScene → upload_fn(Vec<u8>)」的通路，
//!   只是开源构建里整段被 `#[cfg(closed)]` 关掉了（`game.rs:948`）。
//!   我们把它打开、换成自己的载荷格式，管道本身一行不改。
//!
//! 契约见 `docs/规范/数据飞轮契约.md`。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::judge::{Judge, Judgement, PlayResult};

/// 判定数据格式版本。改编码必须 +1（服务端按这个字段选解析器）。
pub const JUDGE_VER: u8 = 1;

/// 紧凑记录里的判定码
pub mod judge_code {
    pub const PERFECT: u8 = 0;
    pub const GOOD: u8 = 1;
    pub const BAD: u8 = 2;
    pub const MISS: u8 = 3;
    /// Hold 的「按下」事件 —— **不是失误**，只表示这一次按到了长条
    pub const HOLD_PRESS: u8 = 4;
}

/// `offset_ms` = 这个值表示**上游没记录真实偏移**，不是 0ms。
///
/// 三条来源（对得上 `prpr/src/judge.rs:827-839`）：
///   · Miss      → 上游硬编码 `0.25`（哨兵，不是偏移）
///   · Drag/Flick→ 上游硬编码 `0.`（这些音符本来就自动判 Perfect）
///   · Hold 按下 → `Err(_)` 分支没有 diff
/// 把这个区分做出来，服务端算「平均早/晚」时才不会把垃圾算进去。
pub const OFFSET_UNKNOWN: i16 = i16::MIN;

/// 一条紧凑判定记录占 8 字节（小端）：
/// ```text
/// +0  u16  line_id     `chart.lines` 的下标
/// +2  u16  note_id     该线内 `notes` 的下标
/// +4  i16  offset_ms   早/晚毫秒；OFFSET_UNKNOWN 表示未知
/// +6  u8   judgement   judge_code::*
/// +7  u8   flags       保留，填 0
/// ```
pub const RECORD_SIZE: usize = 8;

/// 上报载荷。字段名走 snake_case（服务端是 JS，无所谓，但保持一致好排查）。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HuameiPayload {
    pub client_ver: String,
    pub judge_ver: u8,

    pub chart_id: i32,
    pub chart_level: String,
    /// sha256(谱面文件字节) —— 与 Phira 官方 `/chart/{id}/verify-cksum` 同一个算法。
    /// ⚠ 由 **phira 侧**在谱面加载时算好填入（prpr 拿不到原始谱面字节）。
    pub chart_digest: Option<String>,
    pub chart_notes: u32,

    pub score: u32,
    pub accuracy: f64,
    pub max_combo: u32,
    /// [perfect, good, bad, miss]
    pub counts: [u32; 4],
    pub speed: f32,
    pub mods: i32,
    /// 是否开着键盘模式。不拒收（桌面端键盘是正当玩法），但服务端可以据此分层。
    pub keyboard: bool,

    /// 结算时刻，Unix 毫秒
    pub played_at: i64,

    /// base64(紧凑判定块)
    pub judge: String,
}

impl HuameiPayload {
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// 把 `Judge` 里的逐音符判定编成紧凑块。
///
/// ⚠ `judgements` 的第 4 个字段是 `Result<JudgeHit, bool>`：
///   `Ok(hit)`  = 真判定（记 `hit.judgement` 与 `hit.diff`）
///   `Err(b)`   = **Hold 的按下事件**（`b` = 是否落在 Perfect 窗口）
/// 把 `Err` 当失误是最容易犯的错 —— 那会让每个 Hold 都污染热力图。
pub fn encode_judgements(judge: &Judge) -> Vec<u8> {
    let js = judge.judgements.borrow();
    let mut out = Vec::with_capacity(1 + js.len() * RECORD_SIZE);
    out.push(JUDGE_VER);

    for (line_id, note_id, res) in js.iter().map(|it| (it.1, it.2, &it.3)) {
        let (code, offset) = match res {
            Ok(hit) => {
                let code = match hit.judgement {
                    Judgement::Perfect => judge_code::PERFECT,
                    Judgement::Good => judge_code::GOOD,
                    Judgement::Bad => judge_code::BAD,
                    Judgement::Miss => judge_code::MISS,
                };
                (code, offset_to_ms(hit.judgement, hit.diff))
            }
            // Hold 按下：没有偏移
            Err(_) => (judge_code::HOLD_PRESS, OFFSET_UNKNOWN),
        };

        // line_id / note_id 超出 u16 是不可能的（谱面不会有 6.5 万条判定线），
        // 但仍然夹一下，宁可丢信息也不要写出越界数据。
        out.extend_from_slice(&(line_id.min(u16::MAX as u32) as u16).to_le_bytes());
        out.extend_from_slice(&(note_id.min(u16::MAX as u32) as u16).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.push(code);
        out.push(0); // flags 保留
    }
    out
}

/// `diff`（秒）→ 毫秒。
///
/// ⚠ 只有 **Click / Hold** 的 `diff` 是真实偏移；Drag / Flick 上游硬编码 `0.`
/// （`judge.rs:834`），Miss 是 `0.25` 哨兵（`judge.rs:832`）。
/// Drag / Flick 的 `0.` 无法与「真的正中」区分 —— 但服务端拿着谱面副本，
/// 知道哪些 note 是 Drag/Flick，统计时排除即可，所以这里保留 0 不误标为未知。
fn offset_to_ms(judgement: Judgement, diff: f64) -> i16 {
    if matches!(judgement, Judgement::Miss) || !diff.is_finite() {
        return OFFSET_UNKNOWN;
    }
    (diff * 1000.).round().clamp(i16::MIN as f64 + 1., i16::MAX as f64) as i16
}

/// 组装载荷。
///
/// ⚠ `client_ver` 留空、`chart_digest` 留 `None` —— 这两项**由 phira 侧填**
///   （它才知道自己的版本号，也只有它拿得到谱面原始字节）。phira 会反序列化
///   这个载荷、补上两项、再序列化发出去。见 `phira/src/scene/song.rs`。
#[allow(clippy::too_many_arguments)]
pub fn build_payload(
    chart_id: i32,
    chart_level: &str,
    chart_notes: u32,
    result: &PlayResult,
    speed: f32,
    mods: i32,
    keyboard: bool,
    judge_bytes: Vec<u8>,
) -> HuameiPayload {
    HuameiPayload {
        client_ver: String::new(),
        judge_ver: JUDGE_VER,
        chart_id,
        chart_level: chart_level.to_owned(),
        chart_digest: None,
        chart_notes,
        score: result.score,
        accuracy: result.accuracy,
        max_combo: result.max_combo,
        counts: result.counts,
        speed,
        mods,
        keyboard,
        played_at: chrono::Utc::now().timestamp_millis(),
        judge: STANDARD.encode(&judge_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_clamped_and_miss_is_unknown() {
        assert_eq!(offset_to_ms(Judgement::Perfect, 0.0123), 12);
        assert_eq!(offset_to_ms(Judgement::Good, -0.0456), -46);
        // Miss 的 0.25 哨兵不能被当成 250ms
        assert_eq!(offset_to_ms(Judgement::Miss, 0.25), OFFSET_UNKNOWN);
        // 极端值被钳住
        assert_eq!(offset_to_ms(Judgement::Bad, 99.0), i16::MAX);
        assert_eq!(offset_to_ms(Judgement::Bad, f64::NAN), OFFSET_UNKNOWN);
    }

    #[test]
    fn judge_code_is_stable() {
        // 这些数字是**线上协议**，改动等于破坏兼容
        assert_eq!(judge_code::PERFECT, 0);
        assert_eq!(judge_code::GOOD, 1);
        assert_eq!(judge_code::BAD, 2);
        assert_eq!(judge_code::MISS, 3);
        assert_eq!(judge_code::HOLD_PRESS, 4);
        assert_eq!(JUDGE_VER, 1);
        assert_eq!(RECORD_SIZE, 8);
    }
}
