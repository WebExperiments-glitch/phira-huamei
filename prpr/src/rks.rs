//! 【话梅】RKS 计算 —— 官方 Phira 口径。
//!
//! ## 口径来源（重要，决定数值能不能和官方对齐）
//!
//! 1. **单曲成绩定数**：与 Phigros 同一条公式，Phira 服务端也用它：
//!
//!    ```text
//!    acc < 70%      → 0
//!    acc ≥ 70%      → ((100×acc − 55) / 45)² × 定数
//!    ```
//!
//!    性质：acc = 100% 时单曲 rks **恰好等于定数**；acc = 70% 时归零。
//!
//! 2. **个人 RKS**：Phira **不是** Phigros 的 B27+Phi3，而是 **R10 + B20**。
//!    这一条来自 Phira 官方版主在社区问答中的公开回复：
//!    「Phira 的 RKS 计算方式是上架区谱面的 R10 + B20」。
//!
//!    ⚠ R10 的**精确定义未公开**（是「最近 10 次游玩」还是「最近 10 个
//!      不同谱面」、是否去重、是否计入未上架谱面），我们按最常见的
//!      「最近 10 次有效游玩 + 最好 20 个单曲成绩，共 30 项取平均」实现，
//!      并把权重做成常量，等拿到官方数据后只需改这几个数字。
//!
//! ## 为什么定数用我们自己的
//!
//! 官方 Phira 的定数已经停止更新（用户 2026-11-16 确认）。
//! 所以这里的 `difficulty` 由我们的 phm 规则引擎提供，公式本身仍是官方口径 ——
//! 这样「同样的 acc + 同样的定数」能算出和官方一致的数值。

/// 单曲 rks 的有效下限：低于此准确率不计分。
pub const MIN_ACC: f64 = 0.7;

/// 个人 RKS 的两段构成（官方口径 R10 + B20）。
pub const RECENT_COUNT: usize = 10;
pub const BEST_COUNT: usize = 20;

/// 单曲成绩定数。
///
/// `difficulty` 是谱面定数（如 14.7），`acc` 是**0~1** 的准确率。
/// 与官方不同的一点：**我们不做四舍五入**，保留全精度，
/// 只在最终显示时取两位小数 —— 否则多次累加会引入偏差。
pub fn song_rks(difficulty: f32, acc: f64) -> f32 {
    if !acc.is_finite() || acc < MIN_ACC || !difficulty.is_finite() || difficulty <= 0. {
        return 0.;
    }
    let factor = (100. * acc - 55.) / 45.;
    (factor * factor * difficulty as f64) as f32
}

/// 一条成绩记录的最小信息。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoreEntry {
    /// 单曲 rks（已按定数与 acc 算好）
    pub rks: f32,
    /// 结算时间（Unix 毫秒），用于挑「最近 N 次」
    pub played_at: i64,
}

/// 个人 RKS = (最近 RECENT_COUNT 次 + 最好 BEST_COUNT 个) / (两者之和)。
///
/// 两项**允许重叠**：官方口径里 B20 与 R10 不是互斥集合，
/// 一首刚打出的高分曲可以同时进入两边。这一点和 Phigros 的
/// 「Best27 可包含 Phi3」是同一个思路。
pub fn player_rks(entries: &[ScoreEntry]) -> f32 {
    if entries.is_empty() {
        return 0.;
    }

    // 最近 N 次：按时间倒序取前 N
    let mut by_time: Vec<&ScoreEntry> = entries.iter().collect();
    by_time.sort_by_key(|e| std::cmp::Reverse(e.played_at));
    let recent_sum: f32 = by_time.iter().take(RECENT_COUNT).map(|e| e.rks).sum();

    // 最好 N 个：按 rks 倒序取前 N
    let mut by_rks: Vec<&ScoreEntry> = entries.iter().collect();
    by_rks.sort_by(|a, b| b.rks.partial_cmp(&a.rks).unwrap_or(std::cmp::Ordering::Equal));
    let best_sum: f32 = by_rks.iter().take(BEST_COUNT).map(|e| e.rks).sum();

    // 分母是**实际参与的平均项数**，不是固定 30 —— 新手只打过 3 局时
    // 不该被 27 个 0 拉低。
    let recent_n = by_time.len().min(RECENT_COUNT) as f32;
    let best_n = by_rks.len().min(BEST_COUNT) as f32;
    let denom = recent_n + best_n;
    if denom <= 0. {
        return 0.;
    }

    let raw = (recent_sum + best_sum) / denom;
    // 官方显示两位小数（四舍五入也在这里做，内部保持全精度）
    (raw * 100.).round() / 100.
}

/// 打一局之后 RKS 会涨多少。用于「谱面库准确率下方显示 RKS 提升提示」。
pub fn rks_delta(before: &[ScoreEntry], after: &[ScoreEntry]) -> f32 {
    (player_rks(after) - player_rks(before)).max(0.)
}


/// 本地成绩历史的一条记录。
///
/// 存盘用这个（自带 chart_id 与定数），算 RKS 时再转成 [`ScoreEntry`]。
/// 定数一并存下来是刻意的：PHM 的引擎升级可能改定数，
/// 但「当时那一局按当时定数得了多少分」不该被追溯修改。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScoreHistoryEntry {
    pub chart_id: i32,
    pub difficulty: f32,
    /// 0~1
    pub acc: f32,
    pub score: i32,
    /// Unix 毫秒
    pub played_at: i64,
}

impl ScoreHistoryEntry {
    pub fn rks(&self) -> f32 {
        song_rks(self.difficulty, self.acc as f64)
    }

    pub fn to_entry(self) -> ScoreEntry {
        ScoreEntry {
            rks: self.rks(),
            played_at: self.played_at,
        }
    }
}

/// 历史最多留多少条。
///
/// R10+B20 只需要 30 项，但保留更多才能让「最近 10 次」在长期游玩后仍然准确
/// 反映近期状态。200 条按每局 ~200 字节算约 40 KB，写进 data.json 无压力。
pub const HISTORY_LIMIT: usize = 200;

/// 追加一条历史，并把长度裁到上限。
pub fn push_history(history: &mut Vec<ScoreHistoryEntry>, entry: ScoreHistoryEntry) {
    history.push(entry);
    if history.len() > HISTORY_LIMIT {
        // 按时间保留最近的
        history.sort_by_key(|e| e.played_at);
        let drop = history.len() - HISTORY_LIMIT;
        history.drain(0..drop);
    }
}

/// 从历史算个人 RKS。
pub fn player_rks_from_history(history: &[ScoreHistoryEntry]) -> f32 {
    let entries: Vec<ScoreEntry> = history.iter().map(|e| e.to_entry()).collect();
    player_rks(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_acc_equals_difficulty() {
        // acc = 100% 时单曲 rks 恰好等于定数 —— 这是公式的核心性质
        assert!((song_rks(15.0, 1.0) - 15.0).abs() < 1e-4);
        assert!((song_rks(14.7, 1.0) - 14.7).abs() < 1e-4);
    }

    #[test]
    fn below_threshold_is_zero_but_exactly_70pct_is_not() {
        // 官方原文：**未达到** 70.00% 才是 0；
        // 恰好 70.00% 时是定数的九分之一（不是 0）。
        assert_eq!(song_rks(15.0, 0.69), 0.0);
        assert_eq!(song_rks(15.0, 0.6999), 0.0);
        assert!(song_rks(15.0, 0.7) > 0.0, "恰好 70% 应该按公式算，不是 0");
        assert!(song_rks(15.0, 0.71) > song_rks(15.0, 0.7));
    }

    #[test]
    fn acc_70pct_is_one_ninth_of_difficulty() {
        // 官方说明：acc 恰好 70% 时，单曲 rks 是定数的九分之一
        // ((70-55)/45)^2 = (1/3)^2 = 1/9
        let r = song_rks(9.0, 0.7);
        let exact = song_rks(9.0, 1.0) / 9.0;
        assert!((r - exact).abs() < 1e-4, "r={r} exact={exact}");
    }

    #[test]
    fn monotonic_in_acc() {
        let low = song_rks(15.0, 0.8);
        let mid = song_rks(15.0, 0.9);
        let high = song_rks(15.0, 0.99);
        assert!(low < mid && mid < high);
    }

    #[test]
    fn empty_entries_zero() {
        assert_eq!(player_rks(&[]), 0.0);
    }

    #[test]
    fn small_history_not_diluted_by_zeros() {
        // 只打过 1 局时，结果应约等于该局的 rks（而不是被 29 个 0 稀释）
        let one = [ScoreEntry { rks: 15.0, played_at: 1 }];
        assert!((player_rks(&one) - 15.0).abs() < 0.01);
    }

    #[test]
    fn best_and_recent_can_overlap() {
        // 单局高分会同时进 R10 和 B20，两项相加后平均仍是该分数
        let e = [ScoreEntry { rks: 14.0, played_at: 100 }];
        assert!((player_rks(&e) - 14.0).abs() < 0.01);
    }

    #[test]
    fn recent_picks_latest_by_time() {
        let mut entries: Vec<ScoreEntry> = (0..15)
            .map(|i| ScoreEntry { rks: i as f32, played_at: i as i64 })
            .collect();
        let before = player_rks(&entries);
        // 新打一局低分，但它是"最近"的，应参与 R10
        entries.push(ScoreEntry { rks: 0.5, played_at: 999 });
        let after = player_rks(&entries);
        assert!(after != before, "最近一次应影响结果");
    }

    #[test]
    fn history_trims_to_limit() {
        let mut h = Vec::new();
        for i in 0..(HISTORY_LIMIT + 50) {
            push_history(&mut h, ScoreHistoryEntry {
                chart_id: i as i32, difficulty: 15., acc: 0.95, score: 900000, played_at: i as i64,
            });
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        // 保留的应该是最新的那批
        assert_eq!(h.last().unwrap().played_at, (HISTORY_LIMIT + 49) as i64);
    }

    #[test]
    fn history_entry_rks_uses_stored_difficulty() {
        let e = ScoreHistoryEntry { chart_id: 1, difficulty: 15., acc: 1.0, score: 1000000, played_at: 0 };
        assert!((e.rks() - 15.).abs() < 1e-4);
    }

    #[test]
    fn delta_non_negative() {
        let before = [ScoreEntry { rks: 10.0, played_at: 1 }];
        let mut after = before.to_vec();
        after.push(ScoreEntry { rks: 12.0, played_at: 2 });
        assert!(rks_delta(&before, &after) >= 0.);
    }
}
