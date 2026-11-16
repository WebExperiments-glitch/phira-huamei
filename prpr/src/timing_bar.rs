//! 【话梅】实时判定条 —— 参考 PhiraiAd 的 timing_bar.rs 重写为自包含版。
//!
//! 上游 PhiraiAd 的实现依赖 `JudgementRangeProfile`(14 字段) + `Scheme`(自定义判定 399 行)，
//! 我们不需要那两套：判定窗口直接用 judge.rs 已有的 LIMIT_PERFECT/GOOD/BAD，
//! 省掉 400+ 行依赖链，也避免动判定核心。
//!
//! 设计：一条以 0 为中心的轴，按窗口着色分三段（绿/黄/红）；
//! 每个判定留一个会淡出的刻度；中心三角指示器 = 最近 3 次偏差的平滑均值。
//!
//! 【话梅创新】中心下方多一行「早/晚倾向」（◀ 早 N · 晚 N ▶），
//! 复用我们已经采集的 early_kind/late_kind —— PhiraiAd 没有这个，
//! 而且它和我们的结算页偏差条、反作弊 timing 是一脉相承的。

use crate::{
    ext::semi_white,
    judge::{Judgement, TimingEvent, LIMIT_BAD, LIMIT_GOOD, LIMIT_PERFECT},
    ui::Ui,
};
use lyon::{math::point, path::Path};
use macroquad::prelude::*;
use std::collections::VecDeque;

/// 轴的时间跨度（秒）。我们的 BAD 窗口是 0.22s，取 ±0.3s 留点余量。
pub const EXTENT: f64 = 0.30;

#[derive(Clone, Copy, Debug)]
struct Sample {
    difference: f64,
    judgement: Judgement,
    born_at: f64,
}

#[derive(Default)]
pub struct TimingBar {
    samples: VecDeque<Sample>,
    /// [Perfect, 早Good, 晚Good, Bad, Miss]
    pub counts: [u32; 5],
    indicator: f64,
    last_real_time: Option<f64>,
    last_input_time: Option<f64>,
    last_three: VecDeque<f64>,
}

impl TimingBar {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 喂一个判定事件。`real_time` 只用于淡出，不参与判定。
    pub fn record(&mut self, event: &TimingEvent, real_time: f64, retention: f64) {
        self.expire(real_time, retention);

        // 计数：Perfect / 早Good / 晚Good / Bad / Miss
        let idx = match event.judgement {
            Judgement::Perfect => 0,
            Judgement::Good if event.diff < 0. => 1,
            Judgement::Good => 2,
            Judgement::Bad => 3,
            Judgement::Miss => 4,
        };
        self.counts[idx] = self.counts[idx].saturating_add(1);

        if !event.diff.is_finite() {
            return;
        }
        // Miss 的 diff 可能是占位值，夹到轴内；其它判定保留精确值
        let difference = if matches!(event.judgement, Judgement::Miss) {
            event.diff.clamp(-EXTENT, EXTENT)
        } else {
            event.diff
        };
        self.samples.push_back(Sample {
            difference,
            judgement: event.judgement,
            born_at: real_time,
        });
        self.last_input_time = Some(real_time);
        self.last_three.push_back(difference);
        while self.last_three.len() > 3 {
            self.last_three.pop_front();
        }
    }

    fn expire(&mut self, real_time: f64, retention: f64) {
        while self.samples.front().is_some_and(|s| real_time - s.born_at >= retention) {
            self.samples.pop_front();
        }
    }

    /// 最近三次偏差的均值（夹在轴内）。
    fn target(&self) -> f64 {
        if self.last_three.is_empty() {
            return 0.;
        }
        let sum: f64 = self.last_three.iter().sum::<f64>() / self.last_three.len() as f64;
        sum.clamp(-EXTENT, EXTENT)
    }

    /// 每帧调用：空闲 1 秒后指示器平滑回归中心。
    pub fn animate(&mut self, real_time: f64, retention: f64) {
        self.expire(real_time, retention);
        let dt = self.last_real_time.map_or(0., |last| (real_time - last).clamp(0., 0.1));
        self.last_real_time = Some(real_time);
        let idle = self.last_input_time.is_none_or(|last| real_time - last >= 1.);
        let target = if idle { 0. } else { self.target() };
        let response = if idle { 0.25 } else { 0.09 };
        self.indicator += (target - self.indicator) * (1. - (-dt / response).exp());
    }
}

fn judgement_color(j: Judgement) -> Color {
    match j {
        Judgement::Perfect => GREEN,
        Judgement::Good => YELLOW,
        Judgement::Bad | Judgement::Miss => RED,
    }
}

/// 轴上的位置：x=0 是中心，n ∈ [-1,1]。
pub fn position(center_x: f32, center_y: f32, n: f32, scale: f32) -> Vec2 {
    vec2(center_x + n.clamp(-1., 1.) * 0.35 * scale, center_y)
}

fn segment(ui: &mut Ui, a: Vec2, b: Vec2, width: f32, color: Color) {
    let mut path = Path::builder();
    path.begin(point(a.x, a.y));
    path.line_to(point(b.x, b.y));
    path.end(false);
    ui.stroke_path(&path.build(), width, color);
}

/// 画判定条。`early/late` 是话梅新增的全局早晚计数。
pub fn render(ui: &mut Ui, state: &TimingBar, scale: f32, early: u32, late: u32) {
    if scale <= 0. {
        return;
    }
    let cy = ui.top * 0.72; // 屏幕下方，避开 HUD 与判定线
    /* 【话梅诊断】首次渲染打一行坐标 —— 如果条跑到屏幕外，这里一眼能看出来。
       用 AtomicBool 只打一次，不刷屏。 */
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::huamei::diag(format!("timing_bar: ui.top={} cy={} scale={}", ui.top, cy, scale));
    }
    let pos = |n: f64| position(0., cy, n as f32, scale);

    // ── 三段彩色轴：按我们的窗口常量（对称近似）──
    let bands: [(f64, f64, Color); 3] = [
        (-LIMIT_PERFECT, LIMIT_PERFECT, GREEN),
        (-LIMIT_GOOD, LIMIT_GOOD, YELLOW),
        (-LIMIT_BAD, LIMIT_BAD, RED),
    ];
    // 从外到内画，内层的绿覆盖黄的中间，黄覆盖红的中间
    for (lo, hi, c) in bands {
        segment(ui, pos(lo), pos(hi), 0.018 * scale, Color { a: 0.55, ..c });
    }
    // 轴底（细灰线），让浅色段在深色背景上也看得清
    segment(ui, pos(-1.), pos(1.), 0.006 * scale, semi_white(0.25));

    // 中心零线
    let zero = pos(0.);
    segment(ui, zero - vec2(0., 0.014 * scale), zero + vec2(0., 0.014 * scale), 0.003 * scale, WHITE);

    // ── 判定刻度：会随时间淡出 ──
    let now = state.last_real_time.unwrap_or(0.);
    let retention = 3.0_f64;
    for s in &state.samples {
        let p = pos((s.difference / EXTENT).clamp(-1., 1.));
        let mut c = judgement_color(s.judgement);
        c.a *= (1. - ((now - s.born_at).max(0.) / retention) as f32).clamp(0., 1.);
        segment(ui, p - vec2(0., 0.045 * scale), p - vec2(0., 0.026 * scale), 0.006 * scale, c);
    }

    // ── 指示器：平滑跟随最近三次均值 ──
    let p = pos(state.indicator / EXTENT) + vec2(0., 0.024 * scale);
    let mut tri = Path::builder();
    tri.begin(point(p.x, p.y));
    tri.line_to(point(p.x - 0.012 * scale, p.y + 0.018 * scale));
    tri.line_to(point(p.x + 0.012 * scale, p.y + 0.018 * scale));
    tri.end(true);
    ui.fill_path(tri.build().iter(), WHITE);

    /* ── 轴两端标注：让用户知道「左=早、右=晚」──
       原来只有对称的色带，玩家会以为「为什么有两个红两个黄」。
       标出来就不是猜谜了。 */
    let edge_y = cy - 0.075 * scale;
    ui.text("早")
        .pos(-0.36 * scale, edge_y)
        .anchor(0.5, 0.5)
        .size(0.34 * scale)
        .color(Color::from_hex_rgb(0x81d4fa))
        .draw();
    ui.text("晚")
        .pos(0.36 * scale, edge_y)
        .anchor(0.5, 0.5)
        .size(0.34 * scale)
        .color(Color::from_hex_rgb(0xffab91))
        .draw();

    /* ── 计数：每个数字下面带中文小标签 ──
       【话梅改进】PhiraiAd 原版是五个裸数字，两个红（Bad/Miss）两个黄
       （早Good/晚Good）让新玩家完全看不懂。这里：
         · Bad 用橙红、Miss 用深红（都是失误，但不是一回事）
         · 每个数字下面标名字，不用猜
         · 早Good/晚Good 的「早/晚」用蓝/橙区分，和结算页一致 */
    let base_y = cy + 0.10 * scale;
    let c_bad = Color::from_hex_rgb(0xffb74d); // 橙红：打到了但偏太多
    let c_miss = Color::from_hex_rgb(0xe53935); // 深红：根本没打到
    let c_early_g = Color::from_hex_rgb(0x81d4fa); // 早 Good
    let c_late_g = Color::from_hex_rgb(0xffab91); // 晚 Good
    let columns: [(usize, &str, Color); 5] = [
        (3, "Bad", c_bad),
        (1, "早G", c_early_g),
        (0, "Perfect", GREEN),
        (2, "晚G", c_late_g),
        (4, "Miss", c_miss),
    ];
    for (i, (idx, label, c)) in columns.into_iter().enumerate() {
        let x = (i as f32 - 2.) * 0.145 * scale;
        ui.text(state.counts[idx].to_string())
            .pos(x, base_y)
            .anchor(0.5, 0.5)
            .size(0.52 * scale)
            .color(c)
            .draw();
        ui.text(label)
            .pos(x, base_y + 0.075 * scale)
            .anchor(0.5, 0.5)
            .size(0.30 * scale)
            .color(Color { a: 0.8, ..c })
            .draw();
    }

    // ── 【话梅创新】早/晚倾向 ──
    // 用我们已经在采集的 early_kind/late_kind，不用 PhiraiAd 那套自定义判定档位。
    let total = early + late;
    if total > 0 {
        let early_pct = early as f32 / total as f32;
        let label = format!("早 {early}  ·  晚 {late}");
        ui.text(label)
            .pos(0., base_y + 0.20 * scale)
            .anchor(0.5, 0.5)
            .size(0.40 * scale)
            .color(semi_white(0.65))
            .draw();
        // 一条小比例条：蓝=早，橙=晚（和结算页偏差条配色一致）
        let bar_w = 0.28 * scale;
        let bar_y = base_y + 0.25 * scale;
        let mid = (early_pct - 0.5) * bar_w;
        ui.fill_rect(Rect::new(-bar_w / 2., bar_y, bar_w, 0.012 * scale), semi_white(0.15));
        let c_early = Color::from_hex_rgb(0x81d4fa);
        let c_late = Color::from_hex_rgb(0xffab91);
        ui.fill_rect(Rect::new(-bar_w / 2., bar_y, (bar_w / 2. + mid).max(0.), 0.012 * scale), c_early);
        ui.fill_rect(Rect::new(mid, bar_y, (bar_w / 2. - mid).max(0.), 0.012 * scale), c_late);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_indicator_follow_events() {
        let mut bar = TimingBar::default();
        let ev = |j, d| TimingEvent { judgement: j, diff: d, final_note: true };
        bar.record(&ev(Judgement::Perfect, 0.01), 0., 3.);
        bar.record(&ev(Judgement::Good, -0.09), 0.1, 3.);
        bar.record(&ev(Judgement::Good, 0.11), 0.2, 3.);
        assert_eq!(bar.counts, [1, 1, 1, 0, 0]);
        assert!((bar.target() - (0.01 - 0.09 + 0.11) / 3.).abs() < 1e-9);
    }

    #[test]
    fn samples_expire_but_counts_persist() {
        let mut bar = TimingBar::default();
        bar.record(&TimingEvent { judgement: Judgement::Bad, diff: 0.2, final_note: true }, 0., 1.);
        assert_eq!(bar.samples.len(), 1);
        bar.expire(2., 1.);
        assert!(bar.samples.is_empty());
        assert_eq!(bar.counts[3], 1, "计数是整局累计，不该随时间清零");
    }

    #[test]
    fn miss_is_clamped_to_axis() {
        let mut bar = TimingBar::default();
        bar.record(&TimingEvent { judgement: Judgement::Miss, diff: 9.9, final_note: true }, 0., 3.);
        assert_eq!(bar.samples.back().unwrap().difference, EXTENT);
    }
}
