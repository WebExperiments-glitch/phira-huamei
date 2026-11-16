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
//!    **R10 的口径已用官方接口实测确认**（2026-11-16）：
//!
//!    · R10 = 最近 **10 次游玩记录**，不是 10 个不同谱面
//!    · **不去重** —— 同一张谱刷 10 次就占满 10 个槽
//!      （实测 UID 100 的 10 个槽里只有 4 张不同的谱）
//!    · B20 在**入池时**要求上架；查询时不再过滤 ——
//!      谱面后来被取消 ranked，历史成绩仍留在池里并计入总分
//!
//!    总分公式：`(Σ bestPool[≤20] + Σ recentPool[≤10]) / 30`
//!    **分母恒为 30**，不按实际条数归一化。
//!    官方 8 个用户实测零误差（Δ ≤ 1.8e-15）；反证其他分母都差 0.65 以上。
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

    /* ⚠ 分母**恒为 30**，不是实际条数。
       我最初写成「实际参与项数」，理由是「新手不该被 27 个 0 拉低」——
       听起来合理，但与官方口径**相反**：官方就是按 30 归一化。

       实测证据（8 个用户零误差，Δ ≤ 1.8e-15）：
         (B20+R10)/30   ✅
         (B20/20+R10/10)/2  差 0.65
         B20/20             差 1.30
         R10/10             差 2.60
         (B20+R10)/20       差 6.25

       后果要说清：只打过 1 局的新玩家，总分 = 该局 rks / 30，
       看起来低得离谱 —— 这是**官方行为**，不是 bug。 */
    let denom = (RECENT_COUNT + BEST_COUNT) as f32;

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

/// 历史保留上限。
///
/// 【2026-11-16 用户要求】从 200 改为**不裁剪**：所有对局都要留下，
/// 这是 B30 / 进步曲线 / 长期统计的数据基础，删了就找不回来。
///
/// 体积：每条 `ScoreHistoryEntry` 约 24 字节（JSON 里约 90 字节），
/// 打 5000 局约 450 KB —— 对 `data.json` 完全可接受。
/// 这里的 100000 只是一个防爆上限（异常数据/死循环写入的保护），
/// 正常玩家一辈子也到不了。
pub const HISTORY_LIMIT: usize = 100_000;

/// 追加一条历史。
///
/// 正常路径下**不裁剪**；只有超过防爆上限（10 万条）才丢弃最旧的。
pub fn push_history(history: &mut Vec<ScoreHistoryEntry>, entry: ScoreHistoryEntry) {
    history.push(entry);
    if history.len() > HISTORY_LIMIT {
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
    fn single_play_contributes_to_both_pools() {
        /* 官方口径：分母恒为 30。
           而且**同一局会同时进 R10 和 B20 两个池**（两个池不是互斥集合），
           所以只打过 1 局时，总分 = 2×rks/30 = rks/15。

           定数 15、acc 100% 的新玩家，RKS 只有 1.00 ——
           看起来低得离谱，但这就是官方的净值方式
           （实测 8 个用户零误差，Δ ≤ 1.8e-15）。 */
        let one = [ScoreEntry { rks: 15.0, played_at: 1 }];
        assert!((player_rks(&one) - 15.0 * 2.0 / 30.0).abs() < 0.01);
    }

    #[test]
    fn best_and_recent_can_overlap() {
        // 单局会同时进 R10 和 B20 两个池，所以贡献是 2×rks/30
        let e = [ScoreEntry { rks: 14.0, played_at: 100 }];
        assert!((player_rks(&e) - 14.0 * 2.0 / 30.0).abs() < 0.01);
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
    fn history_keeps_all_normal_games() {
        // 【用户要求】所有对局都要保存，不再按 200 条裁剪
        let mut h = Vec::new();
        for i in 0..2000 {
            push_history(&mut h, ScoreHistoryEntry {
                chart_id: i as i32, difficulty: 15., acc: 0.95, score: 900000, played_at: i as i64,
            });
        }
        assert_eq!(h.len(), 2000, "2000 局应该全部保留");
        assert_eq!(h.last().unwrap().played_at, 1999);
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
