//! 【话梅】判定范围可视化 —— 参考 PhiraiAd 的 judgement_range.rs 重写。
//!
//! ## 它在画什么
//!
//! 对每个音符，把它在**判定窗口起点**和**终点**两个时刻的屏幕位置各算一次，
//! 取这段位置差画成一个矩形：宽度 = 音符判定宽度，高度 = 时间窗口换算成的距离。
//! 于是你直接看见「这个音符容错多大、Perfect 区在哪、Good 区到哪」。
//!
//! ## 与 PhiraiAd 的差别
//!
//! 上游依赖 `JudgementRangeProfile`（14 字段，含 tap/hold/drag/flick 各自的
//! early/late 窗口）与 `Scheme`（自定义判定 399 行）。我们不做自定义判定，
//! 直接用 judge.rs 的三个常量 LIMIT_PERFECT/GOOD/BAD —— 少 400+ 行依赖，
//! 也不会因为窗口来源不同而和真实判定漂移。
//!
//! 上游还支持 `Anchor::Note`（相对音符而非判定线）。我们只保留 Line 锚点：
//! 音游里判定线是视觉基准，相对音符的版本更适合 3D 视角，2D 下反而难读。
//!
//! ## 为什么必须按「动画在任意时间点」采样
//!
//! 不能用 `note.y - speed * dt` 这种线性近似：谱面可以有 yControl / sizeControl /
//! posControl，速度本身是随时间变的。直接线性换算出来的范围在带控制器的谱面上
//! **会明显画歪**。所以这里复刻 `Note::render` 的坐标推导，只是把时间参数化。

use crate::{
    core::{AnimFloat, CtrlObject, JudgeLine, Note, NoteKind, Resource},
    judge::{LIMIT_BAD, LIMIT_GOOD, LIMIT_PERFECT},
    parse::RPE_HEIGHT,
};
use macroquad::prelude::*;

/// 矩形最小高度：窗口在屏幕上退化成一条线时仍能看见。
const MIN_RECT_SIZE: f32 = 0.0015;
const OUTLINE_WIDTH: f32 = 0.003;
/// 填充透明度。调试层要能看清**音符本身**，所以压得很低。
const ALPHA: f32 = 0.085;

const PERFECT_COLOR: Color = Color::new(0.30, 0.88, 1.00, 1.0);
const GOOD_COLOR: Color = Color::new(0.42, 0.92, 0.43, 1.0);
const BAD_COLOR: Color = Color::new(1.00, 0.62, 0.18, 1.0);
/// Miss 阈值与 Bad 同界（超出 LIMIT_BAD 即 Miss），用更深的红区分「已经救不回来」。
const MISS_COLOR: Color = Color::new(1.00, 0.30, 0.35, 1.0);

#[inline]
fn sample(anim: &AnimFloat, time: f64, default: f32) -> f32 {
    let mut anim = anim.clone();
    anim.set_time(time);
    anim.now_opt().unwrap_or(default)
}

#[inline]
fn sample_ctrl(anim: &AnimFloat, height: f64, default: f32) -> f32 {
    sample(anim, height, default)
}

/// 复刻 `Note::render` 在任意谱面时间下的行内 y 偏移。
fn point_base_at(
    note: &Note,
    line: &JudgeLine,
    ctrl: &CtrlObject,
    res: &Resource,
    point_height: f64,
    time: f64,
) -> f32 {
    let line_height = sample(&line.height, time, 0.) as f64;
    let y_offset = sample(&note.object.translation.1, time, 0.);
    let ctrl_height = if note.speed.abs() <= f64::EPSILON {
        note.height - line_height
    } else {
        note.height - line_height + y_offset as f64 / note.speed
    } * RPE_HEIGHT as f64
        / 2.;
    let y_ctrl = sample_ctrl(&ctrl.y, ctrl_height, 1.) as f64;
    let spd = note.speed * y_ctrl;
    /* 逐字对齐 `Note::render`：
         let spd = self.speed * ctrl_obj.y
         let base = (self.height - line_height) / res.chart_aspect * spd
         tr.y = object.translation.1 + base
       ⚠ 三个地方与 PhiraiAd 不同，照抄会整体错位：
         · 用 `chart_aspect` 而不是 `aspect_ratio`
           （竖屏铺满不裁切时两者不等，见 Resource 里的注释）
         · yOffset **不再除以 aspect**（项目改坐标系统时一并改了）

       关于流速：原先这里写着「没有 note_flow_speed 字段，流速已揉进 spd」——
       **那是个错误结论**。spd 只等于 note.speed * y_ctrl，从来不含流速。
       2026-11-18 移植了该字段之后必须在这里显式乘上，否则玩家把视觉流速
       调大、音符画得更远，而判定范围框还留在原位 —— 框会整体错位。 */
    let _ = y_offset;
    ((point_height - line_height) / res.chart_aspect as f64 * spd * res.note_flow_speed as f64) as f32
}

/// 音符判定宽度的**屏幕半宽**。
///
/// 上游还要处理 PhigrosReplica 模式（判定区跟屏幕高度走）与 multipleHint
/// 纹理宽度；我们保留 per-chart 的 sizeControl / note scale / judge_area 三项，
/// 这三项是实际会改变判定区的。
fn note_half_width(note: &Note, line: &JudgeLine, ctrl: &CtrlObject, res: &Resource) -> f32 {
    let line_height = line.height.now() as f64;
    let y_offset = note.object.translation.1.now() as f64;
    let ctrl_height = if note.speed.abs() <= f64::EPSILON {
        note.height - line_height
    } else {
        note.height - line_height + y_offset / note.speed
    } * RPE_HEIGHT as f64
        / 2.;
    let ctrl_size = sample_ctrl(&ctrl.size, ctrl_height, 1.).abs();
    let note_size = note.object.scale.now_with_def(1., 1.).x.abs();
    /* `res.note_width` 本身就是半宽（见 Note::render 里 `scale = ... * res.note_width`，
       再交给 draw_center 做居中缩放）。这里乘上 per-chart 的 size 与 judge_area。 */
    let half = res.note_width * note_size * ctrl_size * note.judge_area.abs();
    half.max(MIN_RECT_SIZE)
}

struct Ctx<'a> {
    note: &'a Note,
    line: &'a JudgeLine,
    ctrl: &'a CtrlObject,
    res: &'a Resource,
    half_width: f32,
}

impl Ctx<'_> {
    /// 某个时间窗口对应的矩形。
    fn rect(&self, start: f64, end: f64) -> Rect {
        let side = if self.note.above { 1. } else { -1. };
        let h = self.note.height;
        // 锚在判定线上：算的是「窗口两端相对判定线的位移」
        let at_start = point_base_at(self.note, self.line, self.ctrl, self.res, h, start);
        let at_end = point_base_at(self.note, self.line, self.ctrl, self.res, h, end);
        let y1 = side * at_start;
        let y2 = side * at_end;
        /* x 同样受 posControl 影响（Hold 除外，见 now_transform 里的分支）。
           incline 的横向偏移很小，调试视图下忽略。 */
        let pos_ctrl = self.ctrl.pos.now_opt().unwrap_or(1.);
        let x = self.note.object.translation.0.now()
            * if matches!(self.note.kind, NoteKind::Hold { .. }) { 1. } else { pos_ctrl };
        Rect::new(
            x - self.half_width,
            y1.min(y2),
            self.half_width * 2.,
            (y1 - y2).abs().max(MIN_RECT_SIZE),
        )
    }

    fn fill(&self, start: f64, end: f64, color: Color) {
        if end <= start {
            return;
        }
        let r = self.rect(start, end);
        draw_rectangle(r.x, r.y, r.w, r.h, Color { a: ALPHA, ..color });
    }

    fn outline(&self, start: f64, end: f64, color: Color) {
        let r = self.rect(start, end);
        draw_rectangle_lines(r.x, r.y, r.w, r.h, OUTLINE_WIDTH, color);
    }

    /// 画 `outer - inner` 作为**两条不重叠的带**。
    ///
    /// 直接按「外窗口填一遍、内窗口再填一遍」会让嵌套区透明度翻倍，
    /// 颜色深浅失真 —— 所以必须算差集。这是上游最实用的一处细节。
    fn band(&self, outer: f64, inner: f64, color: Color) {
        if outer <= inner {
            return;
        }
        // 早侧
        self.fill(-outer, -inner, color);
        // 晚侧
        self.fill(inner, outer, color);
    }
}

/// 画一帧的判定范围。`res.time` 是当前谱面时间。
///
/// ## 为什么必须复用引擎矩阵
///
/// 最初我手算屏幕坐标（`point_base_at` 当 y 直接用），结果日志里
/// `y=33.483` —— 屏幕坐标范围只有 ±1.74，矩形被画到屏幕外几十倍远。
///
/// 原因是 `base` 只是**相对判定线**的高度差（RPE 坐标），要变成屏幕坐标
/// 还差一整套变换：判定线的旋转/平移（`JudgeLine::now_transform`）、
/// 音符自身的 translation/scale、以及谱面整体的 y 翻转。
///
/// 所以这里不手算了：直接取引擎给每个音符用的那个矩阵，在矩阵空间里画矩形。
/// 位置由引擎保证，我们只负责「画多高」。
pub fn render(chart: &crate::core::Chart, res: &Resource) {
    let now = res.time;
    let speed = (res.config.speed as f64).max(0.01);

    /* 【性能】一趟遍历直接画，不收集到 Vec。
       之前用 `Vec<Item>` 存下所有矩形再统一绘制，那等于**每帧一次堆分配**
       —— 60fps 下每秒 60 次 malloc/free，在移动端是实打实的开销。
       现在既然坐标已经是屏幕空间（不需要 apply_model_of 变换），
       就可以边算边画，零分配。

       取舍：绘制顺序从「按线分组」变成「按遍历顺序」。
       这些都是半透明叠加层，顺序对观感没有可见影响。 */
    for line in &chart.lines {
        let line_tr = line.now_transform(res, &chart.lines);
        let ctrl = line.ctrl_obj.borrow().clone();
        let incline_sin = line.incline.now_opt().map(|it| it.to_radians().sin()).unwrap_or_default();

        for note in &line.notes {
            if note.fake || !matches!(note.judge, crate::judge::JudgeStatus::NotJudged) {
                continue;
            }
            if note.time + LIMIT_BAD * speed < now - 0.1 || note.time - LIMIT_BAD * speed > now + horizon() {
                continue;
            }

            let half_local = note_half_width(note, line, &ctrl, res);
            let m_center = line_tr * note.now_transform(res, &ctrl, 0., incline_sin);
            let sx = m_center[(0, 0)].abs().max(1e-3);
            let half = (half_local * sx).clamp(MIN_RECT_SIZE, 1.0);
            let cx = m_center[(0, 2)] * if res.config.flip_x() { -1. } else { 1. };

            let mut bands = [(0f32, 0f32); 3];
            for (i, w) in [LIMIT_BAD, LIMIT_GOOD, LIMIT_PERFECT].into_iter().enumerate() {
                let b0 = point_base_at(note, line, &ctrl, res, note.height, note.time - w * speed);
                let b1 = point_base_at(note, line, &ctrl, res, note.height, note.time + w * speed);
                let m0 = line_tr * note.now_transform(res, &ctrl, b0, incline_sin);
                let m1 = line_tr * note.now_transform(res, &ctrl, b1, incline_sin);
                let y_early = -m0[(1, 2)];
                let y_late = -m1[(1, 2)];
                let top = y_early.min(y_late);
                let h = (y_late.max(y_early) - top).max(MIN_RECT_SIZE);
                bands[i] = (top, h);
            }

            // 完全在屏幕外就跳过（避免边界糊成一条带）
            if bands[0].0 + bands[0].1 < -1.9 || bands[0].0 > 1.9 {
                continue;
            }

            /* 只给 Perfect 淡填充（玩家真正要瞄的），Good 极淡，
               Bad / Miss 只画线 —— 整块填充会糊住半个屏幕。 */
            let (ptop, ph) = bands[2];
            draw_rectangle(cx - half, ptop, half * 2., ph, Color { a: ALPHA * 1.5, ..PERFECT_COLOR });
            let (gtop, gh) = bands[1];
            draw_rectangle(cx - half, gtop, half * 2., gh, Color { a: ALPHA * 0.5, ..GOOD_COLOR });

            // Miss 阈值：超出即为 Miss —— 一条红线，告诉你「再偏就没了」
            draw_rectangle(
                cx - half,
                bands[0].0,
                half * 2.,
                OUTLINE_WIDTH * 0.7,
                Color { a: 0.9, ..MISS_COLOR },
            );
            // Perfect 边界圈，玩家最关心的「稳不稳」
            draw_rectangle_lines(cx - half, ptop, half * 2., ph, OUTLINE_WIDTH, Color { a: 0.85, ..PERFECT_COLOR });
        }
    }
}

/// 只画未来多少秒内的音符。
fn horizon() -> f64 {
    /* 1.2 秒：够看清即将到来的音符，又不至于让顶部堆满还没进视野的矩形。 */
    1.2
}

