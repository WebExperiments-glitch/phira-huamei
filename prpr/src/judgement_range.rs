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
         · 没有 `note_flow_speed` 这个字段，流速已经揉进 spd */
    let _ = y_offset;
    ((point_height - line_height) / res.chart_aspect as f64 * spd) as f32
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
pub fn render(chart: &crate::core::Chart, res: &mut Resource) {
    let now = res.time;
    let speed = (res.config.speed as f64).max(0.01);

    /* 直接把「窗口两端」各自算成一个**屏幕坐标**，不再依赖中间矩阵去变换矩形。
       前几版失败的原因都是中间那层：line_tr 带缩放/旋转，note_tr 又带一遍
       translation，矩形被放大到 3.7 倍并飞出屏幕。
       现在取 `line_tr * note_tr` 的平移分量当屏幕坐标，尺寸由两端之差直接得出。 */
    struct Item {
        cx: f32,
        cy: f32,
        half: f32,
        bands: [(f32, f32); 3], // (中心, 高度)
    }
    let mut items: Vec<Item> = Vec::new();

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

            // 局部半宽 × 矩阵的 x 缩放 → 屏幕半宽
            let half_local = note_half_width(note, line, &ctrl, res);
            let m_center = line_tr * note.now_transform(res, &ctrl, 0., incline_sin);
            // 屏幕 y = -矩阵 y（谱面相机 zoom 的 y 是负数）
            let _ = &m_center;
            let sx = m_center[(0, 0)].abs().max(1e-3);
            let half = (half_local * sx).clamp(MIN_RECT_SIZE, 1.0);

            let mut bands = [(0f32, 0f32); 3];
            for (i, w) in [LIMIT_BAD, LIMIT_GOOD, LIMIT_PERFECT].into_iter().enumerate() {
                let b0 = point_base_at(note, line, &ctrl, res, note.height, note.time - w * speed);
                let b1 = point_base_at(note, line, &ctrl, res, note.height, note.time + w * speed);
                let m0 = line_tr * note.now_transform(res, &ctrl, b0, incline_sin);
                let m1 = line_tr * note.now_transform(res, &ctrl, b1, incline_sin);
                let y_early = -m0[(1, 2)]; // 窗口起点（更早 = 更靠上）
                let y_late = -m1[(1, 2)];  // 窗口终点
                /* 【只画判定前的一半】
                   完整窗口是 note.time ± w，所以线上下各 w 的距离。
                   但判定后的一半在视觉上只会让整块往下盖住判定线，
                   玩家真正关心的是「提前多久内有效」。
                   这里以判定时刻的位置（= 线的位置）为底边，向上取一整段。
                   仍在注释里标明：完整窗口是对称的，只是显示只取上半。 */
                let bottom = y_late.max(y_early); // 靠下的那条（线的位置）
                let top = y_early.min(y_late);
                let h = (bottom - top).max(MIN_RECT_SIZE);
                bands[i] = (top, h);
            }

            items.push(Item {
                cx: m_center[(0, 2)] * if res.config.flip_x() { -1. } else { 1. },
                cy: bands[2].0,
                half,
                bands,
            });
        }
    }

    let colors = [BAD_COLOR, GOOD_COLOR, PERFECT_COLOR];
    /* 外层已经套了谱面的 y 翻转（见 render() 的 apply_model_of），
       这里的坐标会随之翻转，所以直接画即可。 */
    for it in &items {
        // 完全在屏幕外的直接跳过（避免边界糊成一条带）
        let top_edge = it.bands[0].0;
        let bottom_edge = it.bands[0].0 + it.bands[0].1;
        if bottom_edge < -1.9 || top_edge > 1.9 {
            continue;
        }
        /* 【只填充内层，外层只描边】
           Bad 窗口 0.44 秒换算成距离后是一大块（实测 0.78 单位 ≈ 310px），
           整块填充会让音符看起来"悬在半空的板"里，玩家反而读不出重点。
           所以：
             · Perfect —— 实心（最该看的东西）
             · Good    —— 淡化填充
             · Bad     —— **不填充**，只在顶端画一条边界线（容错极限） */
        /* 【挡视线的教训】整块填充 Bad 会糊住半个屏幕。
           现在只给 Perfect 淡填充（那是玩家真正要瞄的），
           Good 极淡，Bad / Miss 只画**横线**（容错极限的位置）。 */
        let (ptop, ph) = it.bands[2];
        draw_rectangle(it.cx - it.half, ptop, it.half * 2., ph, Color { a: ALPHA * 1.5, ..PERFECT_COLOR });
        let (gtop, gh) = it.bands[1];
        draw_rectangle(it.cx - it.half, gtop, it.half * 2., gh, Color { a: ALPHA * 0.5, ..GOOD_COLOR });
        let _ = colors;
        // 外框：Bad 边界，一眼看出容错极限
        // Miss 阈值：超出即为 Miss —— 一条红线，告诉你「再偏就没了」
        let (btop, _bh) = it.bands[0];
        draw_rectangle(
            it.cx - it.half,
            btop,
            it.half * 2.,
            OUTLINE_WIDTH * 0.7,
            Color { a: 0.9, ..MISS_COLOR },
        );

        // Perfect 边界单独描一圈 —— 这是玩家最关心的「稳不稳」
        let (ptop, ph) = it.bands[2];
        draw_rectangle_lines(
            it.cx - it.half,
            ptop,
            it.half * 2.,
            ph,
            OUTLINE_WIDTH,
            Color { a: 0.85, ..PERFECT_COLOR },
        );
    }

    static TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n % 300 == 0 {
        let f = items.first().map_or_else(|| "none".to_owned(), |it| {
            format!(
                "cx={:.3} cy={:.3} half={:.3} badH={:.3}",
                it.cx, it.cy, it.half, it.bands[0].1
            )
        });
        crate::huamei::diag(format!(
            "jrange: t={:.2} notes={} drawn={} first={}",
            now,
            chart.lines.iter().map(|l| l.notes.len()).sum::<usize>(),
            items.len(),
            f
        ));
    }
}

/// 只画未来多少秒内的音符。
fn horizon() -> f64 {
    /* 1.6 秒：够看清即将到来的音符，又不至于让顶部堆满还没进视野的矩形
       （之前 3 秒时屏幕上沿会糊成一条色带）。 */
    1.2
}

