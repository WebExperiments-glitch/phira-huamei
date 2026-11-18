//! Judgement system

use crate::{
    config::Config,
    core::{BadNote, Chart, NoteKind, Point, Resource, Vector, NOTE_WIDTH_RATIO_BASE},
    ext::{get_viewport, NotNanExt},
};
use macroquad::prelude::{
    utils::{register_input_subscriber, repeat_all_miniquad_input},
    *,
};
use miniquad::{EventHandler, MouseButton};
use once_cell::sync::Lazy;
use sasa::{PlaySfxParams, Sfx};
use serde::Serialize;
use std::{cell::RefCell, collections::HashMap, mem, num::FpCategory};
use tracing::debug;

pub const FLICK_SPEED_THRESHOLD: f32 = 0.8;
pub const LIMIT_PERFECT: f64 = 0.08;
pub const LIMIT_GOOD: f64 = 0.16;
pub const LIMIT_BAD: f64 = 0.22;
pub const UP_TOLERANCE: f64 = 0.05;
pub const DIST_FACTOR: f64 = 0.2;

const EARLY_OFFSET: f64 = 0.07;

#[derive(Debug, Clone)]
pub enum HitSound {
    None,
    Click,
    Flick,
    Drag,
    Custom(String),
}

impl HitSound {
    pub fn play(&self, res: &mut Resource) {
        match self {
            HitSound::None => {}
            HitSound::Click => play_sfx(&mut res.sfx_click, &res.config),
            HitSound::Flick => play_sfx(&mut res.sfx_flick, &res.config),
            HitSound::Drag => play_sfx(&mut res.sfx_drag, &res.config),
            HitSound::Custom(s) => {
                if let Some(sfx) = res.extra_sfxs.get_mut(s) {
                    play_sfx(sfx, &res.config);
                }
            }
        }
    }

    pub fn default_from_kind(kind: &NoteKind) -> Self {
        match kind {
            NoteKind::Click => HitSound::Click,
            NoteKind::Flick => HitSound::Flick,
            NoteKind::Drag => HitSound::Drag,
            NoteKind::Hold { .. } => HitSound::Click,
        }
    }
}

pub fn play_sfx(sfx: &mut Sfx, config: &Config) {
    if config.volume_sfx <= 1e-2 {
        return;
    }
    let _ = sfx.play(PlaySfxParams {
        amplifier: config.volume_sfx,
    });
}

#[cfg(all(not(target_os = "windows"), not(target_os = "ios")))]
fn get_uptime() -> f64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    assert!(ret == 0);
    time.tv_sec as f64 + time.tv_nsec as f64 * 1e-9
}

#[cfg(target_os = "ios")]
fn get_uptime() -> f64 {
    objc2_foundation::NSProcessInfo::processInfo().systemUptime()
}

#[cfg(target_os = "windows")]
fn get_uptime() -> f64 {
    miniquad::native::windows::get_uptime()
}

pub struct FlickTracker {
    threshold: f32,
    last_point: Point,
    last_delta: Option<Vector>,
    last_time: f32,
    flicked: bool,
    stopped: bool,
}

impl FlickTracker {
    pub fn new(_dpi: u32, time: f32, point: Point) -> Self {
        // TODO maybe a better approach?
        let dpi = 275;
        Self {
            threshold: FLICK_SPEED_THRESHOLD * dpi as f32 / 386.,
            last_point: point,
            last_delta: None,
            last_time: time,
            flicked: false,
            stopped: true,
        }
    }

    pub fn push(&mut self, time: f32, position: Point) {
        let delta = position - self.last_point;
        self.last_point = position;
        if let Some(last_delta) = &self.last_delta {
            let dt = time - self.last_time;
            let speed = delta.dot(last_delta) / dt;
            if speed < self.threshold {
                self.stopped = true;
            }
            if self.stopped && !self.flicked {
                self.flicked = delta.magnitude() / dt >= self.threshold * 2.;
            }
            // if speed < self.threshold || self.stopped {
            // self.stopped = delta.magnitude() / dt < self.threshold * 5.;
            // self.flicked = self.threshold <= speed;
            // if self.flicked {
            // warn!("new flick!");
            // }
            // }
        }
        self.last_delta = Some(delta.normalize());
        self.last_time = time;
    }
}

#[derive(Debug)]
pub enum JudgeStatus {
    NotJudged,
    PreJudge,
    Judged,
    Hold(bool, f64, f64, bool, f64), // perfect, at, diff, pre-judge, up-time
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, Serialize)]
pub enum Judgement {
    Perfect,
    Good,
    Bad,
    Miss,
}

#[cfg(not(closed))]
#[derive(Default)]
pub(crate) struct JudgeInner {
    diffs: Vec<f64>,

    combo: u32,
    max_combo: u32,
    counts: [u32; 4],
    num_of_notes: u32,
    early_kind: [u32; 4],
    late_kind: [u32; 4],

    /// 本局出现过的**最大同时触点数**。
    /// 用来区分「双指流」和「多指分片」——同样 100 万分的两个人，
    /// 指法结构可能完全不同，这是判定数据给不出的信息。
    /// 每帧在 Judge::update 里更新（取 events.len() 的峰值）。
    max_touch: u32,
}

#[cfg(not(closed))]
impl JudgeInner {
    pub fn new(num_of_notes: u32) -> Self {
        Self {
            diffs: Vec::new(),

            combo: 0,
            max_combo: 0,
            counts: [0; 4],
            num_of_notes,
            early_kind: [0; 4],
            late_kind: [0; 4],
            max_touch: 0,
        }
    }

    pub fn commit(&mut self, what: Judgement, diff: f64) {
        use Judgement::*;
        /* 【话梅 2026-11-16 修】`diffs` 原来**只收 Good** —— 而 early_kind/late_kind
           统计的是**所有判定**。两套口径不一致，于是结算页会出现
           「早 209 / 晚 130，误差 ±0ms」这种自相矛盾的组合：

             这局只有 1 个 Good → diffs 只有 1 个样本 → n < 2 → std 直接给 0。
             但早/晚是 576 个 Perfect 一起统计出来的。

           现在统一：收所有**非零**偏差（Drag/Flick 的硬编码 0 排除，
           零偏移既不算早也不算晚，进 diffs 只会拉低标准差、让结果失真）。

           影响面（已确认）：
             · result().std   —— 从「Good 的离散度」变成「全谱偏差离散度」，语义更正确
             · result().early/late —— 同源，与结算页的分档早晚条口径一致
             · 上报的 timing/early/late 字段 —— 数值会变，但这是**修正**而非破坏；
               历史数据仍可反序列化，服务端按 judged_ver 分层即可 */
        if diff != 0. {
            self.diffs.push(diff);
        }
        if diff < 0. {
            self.early_kind[what as usize] += 1;
        } else if diff > 0. {
            self.late_kind[what as usize] += 1;
        }
        self.counts[what as usize] += 1;
        match what {
            Perfect | Good => {
                self.combo += 1;
                if self.combo > self.max_combo {
                    self.max_combo = self.combo;
                }
            }
            _ => {
                self.combo = 0;
            }
        }
    }

    pub fn reset(&mut self) {
        self.combo = 0;
        self.max_combo = 0;
        self.counts = [0; 4];
        self.diffs.clear();
        self.early_kind = [0; 4];
        self.late_kind = [0; 4];
    }

    pub fn accuracy(&self) -> f64 {
        (self.counts[0] as f64 + self.counts[1] as f64 * 0.65) / self.num_of_notes as f64
    }

    pub fn real_time_accuracy(&self) -> f64 {
        let cnt = self.counts.iter().sum::<u32>();
        if cnt == 0 {
            return 1.;
        }
        (self.counts[0] as f64 + self.counts[1] as f64 * 0.65) / cnt as f64
    }

    pub fn score(&self) -> u32 {
        const TOTAL: u32 = 1000000;
        if self.counts[0] == self.num_of_notes {
            TOTAL
        } else {
            let score = (0.9 * self.accuracy() + self.max_combo as f64 / self.num_of_notes as f64 * 0.1) * TOTAL as f64;
            score.round() as u32
        }
    }

    pub fn result(&self) -> PlayResult {
        /* ⚠⚠ 这里原来写的是 `std: 0.` —— 于是结算界面上那个「ERROR ±Nms」
           **永远显示 ±0**。它不是"这一局很准"，是压根没算。
           现在真算出来：`diffs` 的**总体**标准差（不是样本标准差 ——
           这是这一局的全部数据，没有"用样本估计总体"的意思）。

           ⚠ 口径（2026-11-16 已统一）：`diffs` 现在收**所有判定的非零偏差**。
             曾经只收 Good，导致「早 209/晚 130 却显示 ±0ms」——
             这局只有 1 个 Good，样本不足直接归零。
             现在 std 与 early/late 来自同一份数据，不会再自相矛盾。 */
        let timed: Vec<f64> = self.diffs.iter().copied().filter(|d| *d != 0.).collect();
        let n = timed.len();
        let std = if n < 2 {
            0.
        } else {
            let mean = timed.iter().sum::<f64>() / n as f64;
            (timed.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n as f64).sqrt() as f32
        };
        let early = timed.iter().filter(|d| **d < 0.).count() as u32;
        PlayResult {
            score: self.score(),
            accuracy: self.accuracy(),
            max_combo: self.max_combo,
            num_of_notes: self.num_of_notes,
            counts: self.counts,
            early,
            /* ⚠ 原来写的是 `diffs.len() - early`，那会把 diff == 0 的
               （Drag/Flick 的硬编码 0）算成"晚"。这里按 timed 取，
               零偏移的既不算早也不算晚。 */
            late: n as u32 - early,
            std,
            early_kind: self.early_kind,
            late_kind: self.late_kind,
            max_touch: self.max_touch,
        }
    }

    pub fn combo(&self) -> u32 {
        self.combo
    }

    pub fn counts(&self) -> [u32; 4] {
        self.counts
    }

    /// 【话梅】早/晚计数（判定条的创新行用）。排除 Miss，与结算页口径一致。
    pub fn early_late_counts(&self) -> (u32, u32) {
        let early: u32 = self.early_kind[0..3].iter().sum();
        let late: u32 = self.late_kind[0..3].iter().sum();
        (early, late)
    }
}

#[rustfmt::skip]
#[cfg(closed)]
pub mod inner;
#[cfg(closed)]
use inner::*;

/// 一次**真判定**的完整记录。
///
/// 【huamei 改动】上游这里原本只存 `Judgement`，把 `diff`（判定时刻相对音符
/// 时刻的偏移，秒，负=早 正=晚）丢掉了 —— 而偏移正是「为什么失分」的关键信号
/// （整段偏早 = 读谱问题；零散偏晚 = 手速问题）。
/// 现在把它和判定绑在同一个结构体里，而不是另开一个并行数组：
/// 多人模式会 `drain(..)` 掉 `judgements`，并行数组会在那里失同步。
#[derive(Clone, Copy, Debug)]
pub struct JudgeHit {
    pub judgement: Judgement,
    pub diff: f64,
}

/// 【话梅】给实时判定条用的轻量事件。
///
/// 和 `judgements`（结算上报用）分开：那条链要完整保留、序列化、和服务端对齐；
/// 判定条只要「最近几次的偏差」，每帧 drain 掉即可。混用会在多人模式
/// `drain(..)` 时失同步（见上面 Judgements 的注释）。
#[derive(Clone, Copy, Debug)]
pub struct TimingEvent {
    pub judgement: Judgement,
    pub diff: f64,
    /// true = Hold 头部之外的「判定完成」事件（含 Hold 尾判）
    pub final_note: bool,
}

type Judgements = Vec<(f64, u32, u32, Result<JudgeHit, bool>)>;

#[repr(C)]
pub struct Judge {
    // notes of each line in order
    // LinkedList::drain_filter is unstable...
    pub notes: Vec<(Vec<u32>, usize)>,
    pub trackers: HashMap<u64, FlickTracker>,
    pub last_time: f64,

    key_down_count: u32,

    pub(crate) inner: JudgeInner,
    pub judgements: RefCell<Judgements>,
    /// 判定条专用事件队列（每帧被 game.rs drain）。不参与上报。
    timing_events: RefCell<Vec<TimingEvent>>,
}

#[derive(Default)]
struct TouchStatus {
    touches: Vec<Touch>,
    key_delta: i32,
    keys_down: u32,
}

static SUBSCRIBER_ID: Lazy<usize> = Lazy::new(register_input_subscriber);

/// 【话梅 2026-11-17】请求清空触摸状态。
///
/// 根因：`on_new_frame` 每帧从 miniquad 重放输入事件来重建 TOUCHES，
/// 而 `Handler::finalize` **只补鼠标**，从不清理「Started 了但没等到 Ended」
/// 的触摸。切后台 / 暂停 / 场景切换时 Ended 事件会丢，那个触点就永远留下：
///
///   · 位置固定不动 → UI 的 update_long_touch 认为「手指一直按着」
///     → 长按计时器走到底 → **短按却弹出长按菜单**
///   · 反复几次累积多个残留 → 一帧内多个触点同时命中不同谱面
///     → **三个谱子一起响应**（用户实测）
///
/// 物理事实：切后台时手指必然离开屏幕（或系统已取消触摸），
/// 所以恢复后把状态归零是安全且正确的。
static TOUCH_RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 由 UI 线程（切后台回调）调用，下一帧生效。
pub fn request_touch_reset() {
    TOUCH_RESET.store(true, std::sync::atomic::Ordering::Release);
}
thread_local! {
    static TOUCHES: RefCell<TouchStatus> = RefCell::default();
    static WHEEL: RefCell<(f32, f32)> = RefCell::default();
}

pub fn take_wheel() -> (f32, f32) {
    WHEEL.with(|it| mem::take(&mut *it.borrow_mut()))
}

impl Judge {
    pub fn new(chart: &Chart) -> Self {
        let notes = chart
            .lines
            .iter()
            .map(|line| {
                let mut idx: Vec<u32> = (0..(line.notes.len() as u32)).filter(|it| !line.notes[*it as usize].fake).collect();
                idx.sort_by_key(|id| line.notes[*id as usize].time.not_nan());
                (idx, 0)
            })
            .collect();
        Self {
            notes,
            trackers: HashMap::new(),
            last_time: 0.,

            key_down_count: 0,

            inner: JudgeInner::new(chart.lines.iter().map(|it| it.notes.iter().filter(|it| !it.fake).count() as u32).sum()),
            judgements: RefCell::new(Vec::new()),
            timing_events: RefCell::new(Vec::new()),
        }
    }

    pub fn reset(&mut self) {
        self.notes.iter_mut().for_each(|it| it.1 = 0);
        self.trackers.clear();
        self.inner.reset();
        self.judgements.borrow_mut().clear();
        self.timing_events.borrow_mut().clear();
    }

    /// Advance note pointers past notes before time `t`, marking them as judged.
    /// Used in exercise mode to skip notes before the exercise range start.
    pub fn advance_to(&mut self, chart: &mut Chart, t: f64) {
        for (line, (idx, st)) in chart.lines.iter_mut().zip(self.notes.iter_mut()) {
            while *st < idx.len() {
                let note = &mut line.notes[idx[*st] as usize];
                if note.time >= t {
                    break;
                }
                note.judge = JudgeStatus::Judged;
                *st += 1;
            }
        }
        self.last_time = t;
    }

    pub fn commit(&mut self, t: f64, what: Judgement, line_id: u32, note_id: u32, diff: f64) {
        // 【huamei 改动】把 diff 一起留下（上游只存 what）
        self.judgements.borrow_mut().push((
            t,
            line_id,
            note_id,
            Ok(JudgeHit {
                judgement: what,
                diff,
            }),
        ));
        self.timing_events.borrow_mut().push(TimingEvent {
            judgement: what,
            diff,
            final_note: true,
        });
        self.inner.commit(what, diff);
    }

    /// 取走判定条事件（每帧调用）。空队列时不分配。
    pub fn take_timing_events(&self) -> Vec<TimingEvent> {
        std::mem::take(&mut *self.timing_events.borrow_mut())
    }

    #[inline]
    pub fn accuracy(&self) -> f64 {
        self.inner.accuracy()
    }

    #[inline]
    pub fn real_time_accuracy(&self) -> f64 {
        self.inner.real_time_accuracy()
    }

    #[inline]
    pub fn score(&self) -> u32 {
        self.inner.score()
    }

    pub(crate) fn on_new_frame() {
        /* 【话梅】切后台恢复后的第一帧：把触摸状态归零，
           并**跳过本帧的事件重放** —— 队列里可能还压着切走前的
           Started 事件，重放会把幽灵触点又装回去。 */
        if TOUCH_RESET.swap(false, std::sync::atomic::Ordering::AcqRel) {
            TOUCHES.with(|it| *it.borrow_mut() = TouchStatus::default());
            WHEEL.with(|it| *it.borrow_mut() = (0., 0.));
            return;
        }
        let mut handler = Handler {
            status: TouchStatus::default(),
            wheel: (0., 0.),
        };
        repeat_all_miniquad_input(&mut handler, *SUBSCRIBER_ID);
        handler.finalize();
        TOUCHES.with(|it| {
            *it.borrow_mut() = handler.status;
        });
        WHEEL.with(|it| {
            *it.borrow_mut() = handler.wheel;
        });
    }

    fn touch_transform(flip_x: bool) -> impl Fn(&mut Touch) {
        let vp = get_viewport();
        move |touch| {
            let p = touch.position;
            touch.position = vec2(
                (p.x - vp.0 as f32) / vp.2 as f32 * 2. - 1.,
                ((p.y - (screen_height() - (vp.1 + vp.3) as f32)) / vp.3 as f32 * 2. - 1.) / (vp.2 as f32 / vp.3 as f32),
            );
            if flip_x {
                touch.position.x *= -1.;
            }
        }
    }

    /// 【话梅 2026-11-18】从 PhiraiAd 移植：带**倒打**与**练习视野**的输入映射。
    ///
    /// ## 数学
    ///
    /// 倒打 = 画面转 180°（x、y 同时取反），不是一个轴的镜像。
    /// 所以这里用 `rotation` 统一乘 xy，再用 `flip_x ^ flip_y` 决定是否
    /// 再镜像回来 —— 这样四种组合（无/仅X/仅Y/XY）都能正确落到
    /// 「旋转 180°」或「单轴镜像」上。
    ///
    /// ## 安全性（默认状态必须与改造前完全一致）
    ///
    /// `flip_y = false` 且 `view = Default` 时：
    ///   · rotation = 1
    ///   · chart_flip = flip_x
    ///   · PracticeView::default() 的 screen_to_chart 是恒等
    ///     （scale_percent=100 → scale=1，center=(0,0)）
    /// 结果等价于原来的 `touch_transform(flip_x)` —— 有单元测试盯着。
    /// 倒打/镜像的**纯数学**部分（不碰 GL，可单测）。
    ///
    /// 参数 `flip_x` 是**已经 XOR 过 auto_flip_y 的值**，见调用处注释。
    pub(crate) fn chart_touch_math(
        flip_x: bool,
        flip_y: bool,
        view: crate::practice_view::PracticeView,
        x: f32,
        y: f32,
        width: f32,
    ) -> (f32, f32) {
        let rotation = if flip_y { -1. } else { 1. };
        let chart_flip = if flip_x ^ flip_y { -1. } else { 1. };
        let (cx, cy) = view.screen_to_chart(x * rotation, y * rotation, width);
        (cx * chart_flip, cy)
    }

    fn chart_touch_transform(
        flip_x: bool,
        flip_y: bool,
        view: crate::practice_view::PracticeView,
    ) -> impl Fn(&mut Touch) {
        let transform = Self::touch_transform(false);
        let width = get_viewport().2 as f32;
        move |touch| {
            transform(touch);
            let p = touch.position;
            let (x, y) = Self::chart_touch_math(flip_x, flip_y, view, p.x, p.y, width);
            touch.position = vec2(x, y);
        }
    }

    pub fn get_touches() -> Vec<Touch> {
        TOUCHES.with(|it| {
            let guard = it.borrow();
            let tr = Self::touch_transform(false);
            guard
                .touches
                .iter()
                .cloned()
                .map(|mut it| {
                    tr(&mut it);
                    it
                })
                .collect()
        })
    }

    pub fn update(&mut self, res: &mut Resource, chart: &mut Chart, bad_notes: &mut Vec<BadNote>) {
        if res.config.autoplay() {
            self.auto_play_update(res, chart);
            return;
        }
        const X_DIFF_MAX: f64 = 0.21 / (16. / 9.) * 2.;
        let spd = res.config.speed as f64;

        let uptime = get_uptime();

        let t = res.time;
        // TODO optimize
        let mut touches: HashMap<u64, Touch> = {
            let mut touches = touches();
            let btn = MouseButton::Left;
            let id = button_to_id(btn);
            if is_mouse_button_pressed(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Started,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            } else if is_mouse_button_down(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Moved,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            } else if is_mouse_button_released(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Ended,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            }
            /* 【话梅 2026-11-18】改用带倒打/视野的版本。
               当前 auto_flip_y=false 且 practice_view=Default，
               数学上与原来的 touch_transform(flip_x) 完全等价
               （见 chart_touch_transform 的注释），所以手感应无变化。 */
            /* ⚠ 第一个参数必须**先 XOR** auto_flip_y，这是上游的写法，
               不是笔误。展开看就明白了：
                 chart_flip = (config.flip_x ^ auto_flip_y) ^ auto_flip_y
                            = config.flip_x
               也就是说：最终的水平镜像**只由玩家设置决定**，倒打那一步
               只负责 180° 旋转（rotation）。若直接把 config.flip_x 传进去，
               开倒打时会多翻一次水平轴 —— 判定方向整体镜像，手指全按反。
               （我第一版就这么写错了，靠对照上游调用才抓回来。） */
            let flip_x = res.config.flip_x() ^ res.auto_flip_y;
            let tr = Self::chart_touch_transform(flip_x, res.auto_flip_y, res.practice_view);
            touches
                .into_iter()
                .map(|mut it| {
                    tr(&mut it);
                    (it.id, it)
                })
                .collect()
        };
        let (events, keys_down, key_delta) = TOUCHES.with(|it| {
            let guard = it.borrow();
            let events = guard.touches.clone();
            if res.config.use_keyboard {
                (events, guard.keys_down, guard.key_delta)
            } else {
                (events, 0, 0)
            }
        });
          /* 本帧**同时按下的手指数**取峰值（不只是事件数）。
           *
           * 【2026-11-15 修】原来写的是 `events.len()` —— 那是「本帧的
           *   触点**事件**数」，不是手指数。同一个手指一帧内会有多个
           *   事件（按下 + 移动），而 events 是整帧累积的，于是打一局
           *   下来 max 会飙到 23（iPad 最多 10 指，实测最高 5）。
           *
           *   正确做法：按 id 去重，并排除已经 Ended 的手指。
           *   Ended 事件还在 events 里（那是本帧结束的），但它已经不算
           *   「同时按下」了。
           *
           * 键盘模式不算 —— 键盘是 0 个触点，混进来会把四指玩家拉低。 */
          if !res.config.use_keyboard && !events.is_empty() {
              let active: std::collections::HashSet<u64> = events
                  .iter()
                  .filter(|t| t.phase != TouchPhase::Ended)
                  .map(|t| t.id)
                  .collect();
              self.inner.max_touch = self.inner.max_touch.max(active.len() as u32);
          }
        self.key_down_count = self.key_down_count.saturating_add_signed(key_delta);
        /* 像素 → 谱面坐标的纵向系数。
           ⚠ 屏幕上的相对位置对应多少**谱面距离**，取决于相机（视图比例）；
             而谱面自身用的是 `chart_aspect`。横屏两者相等（系数 1，
             与旧行为逐字节一致），竖屏铺满时才不等于 1 ——
             漏了这个系数，竖屏判定会整体偏移。 */
        let y_squash = res.chart_aspect / res.aspect_ratio;
        {
            fn to_local(Vec2 { x, y }: Vec2) -> Point {
                Point::new(x / screen_width() * 2. - 1., y / screen_height() * 2. - 1.)
            }
            let delta = (t / spd - self.last_time) / (events.len() + 1) as f64;
            let mut t = self.last_time;
            for Touch {
                id,
                phase,
                position: p,
                time,
            } in events.into_iter()
            {
                t += delta;
                let t = t as f32;
                let p = to_local(p);
                match phase {
                    TouchPhase::Started => {
                        self.trackers.insert(id, FlickTracker::new(res.dpi, t, p));
                        touches
                            .entry(id)
                            .or_insert_with(|| Touch {
                                id,
                                phase: TouchPhase::Started,
                                position: vec2(p.x, p.y),
                                time,
                            })
                            .phase = TouchPhase::Started;
                    }
                    TouchPhase::Moved | TouchPhase::Stationary => {
                        if let Some(tracker) = self.trackers.get_mut(&id) {
                            tracker.push(t, p);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.trackers.remove(&id);
                    }
                }
            }
        }
        let touches: Vec<Touch> = touches
            .into_values()
            .map(|mut it| {
                it.time = if it.time.is_infinite() {
                    f64::NEG_INFINITY
                } else {
                    t - (uptime - it.time) * spd
                };
                it
            })
            .collect();
        // pos[line][touch]
        let mut pos = Vec::<Vec<Option<Point>>>::with_capacity(chart.lines.len());
        for id in 0..chart.lines.len() {
            chart.lines[id].object.set_time(t);
            let inv = chart.lines[id].now_transform(res, &chart.lines).try_inverse().unwrap();
            pos.push(
                touches
                    .iter()
                    .map(|touch| {
                        let p = touch.position;
                        let p = inv.transform_point(&Point::new(p.x, -p.y * y_squash));
                        fn ok(f: f32) -> bool {
                            matches!(f.classify(), FpCategory::Zero | FpCategory::Subnormal | FpCategory::Normal)
                        }
                        if ok(p.x) && ok(p.y) {
                            Some(p)
                        } else {
                            None
                        }
                    })
                    .collect(),
            );
        }
        let time_of = |touch: &Touch| {
            if touch.time.is_infinite() {
                t
            } else {
                touch.time
            }
        };
        let mut judgements = Vec::new();
        // clicks & flicks
        for (id, touch) in touches.iter().enumerate() {
            let click = touch.phase == TouchPhase::Started;
            let flick =
                matches!(touch.phase, TouchPhase::Moved | TouchPhase::Stationary) && self.trackers.get_mut(&touch.id).is_some_and(|it| it.flicked);
            if !(click || flick) {
                continue;
            }
            let t = time_of(touch);
            let mut closest = (None, X_DIFF_MAX, LIMIT_BAD, LIMIT_BAD + (X_DIFF_MAX / NOTE_WIDTH_RATIO_BASE - 1.).max(0.) * DIST_FACTOR);
            for (line_id, ((line, pos), (idx, st))) in chart.lines.iter_mut().zip(pos.iter()).zip(self.notes.iter_mut()).enumerate() {
                let Some(pos) = pos[id] else {
                    continue;
                };
                for id in &idx[*st..] {
                    let note = &mut line.notes[*id as usize];
                    if !matches!(note.judge, JudgeStatus::NotJudged | JudgeStatus::PreJudge) {
                        continue;
                    }
                    if !click && matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. }) {
                        continue;
                    }
                    let dt = (note.time - t) / spd;
                    if dt >= closest.3 {
                        break;
                    }
                    let dt = if dt < 0. { (dt + EARLY_OFFSET).min(0.).abs() } else { dt };
                    let x = &mut note.object.translation.0;
                    x.set_time(t);
                    let dist = (x.now() - pos.x).abs() as f64 / note.judge_area as f64;
                    if dist > X_DIFF_MAX {
                        continue;
                    }
                    if dt
                        > if matches!(note.kind, NoteKind::Click) {
                            LIMIT_BAD - LIMIT_PERFECT * (dist - 0.9).max(0.)
                        } else {
                            LIMIT_GOOD
                        }
                    {
                        continue;
                    }
                    let dt = if matches!(note.kind, NoteKind::Flick | NoteKind::Drag) {
                        dt + LIMIT_GOOD
                    } else {
                        dt
                    };
                    let key = dt + (dist / NOTE_WIDTH_RATIO_BASE - 1.).max(0.) * DIST_FACTOR;
                    if key < closest.3 {
                        closest = (Some((line_id, *id)), dist, dt, key);
                    }
                }
            }
            if let (Some((line_id, id)), _, dt, _) = closest {
                let line = &mut chart.lines[line_id];
                if matches!(line.notes[id as usize].kind, NoteKind::Drag) {
                    debug!("reject by drag");
                    continue;
                }
                if click {
                    // click & hold
                    let note = &mut line.notes[id as usize];
                    if matches!(note.kind, NoteKind::Flick) {
                        continue; // to next loop
                    }
                    if dt <= LIMIT_GOOD || matches!(note.kind, NoteKind::Hold { .. }) {
                        match note.kind {
                            NoteKind::Click => {
                                note.judge = JudgeStatus::Judged;
                                judgements.push((if dt <= LIMIT_PERFECT { Judgement::Perfect } else { Judgement::Good }, line_id, id, Some(t)));
                            }
                            NoteKind::Hold { .. } => {
                                note.hitsound.play(res);
                                self.judgements.borrow_mut().push((t, line_id as _, id, Err(dt <= LIMIT_PERFECT)));
                                note.judge = JudgeStatus::Hold(dt <= LIMIT_PERFECT, t, t, false, f64::INFINITY);
                            }
                            _ => unreachable!(),
                        };
                    } else {
                        // prevent extra judgements
                        if matches!(note.judge, JudgeStatus::NotJudged) {
                            // keep the note after bad judgement
                            line.notes[id as usize].judge = JudgeStatus::PreJudge;
                            /* 【上游 issue #929】Bad 必须记下触点时间。
                               Good 传的是 Some(t)（t = time_of(touch)，即触点真实时间），
                               Bad 原来传 None → 结算时退化成 `diff.unwrap_or(t)`，
                               用的是**处理这一帧**的时间。卡顿时一帧晚到，
                               本来 Early 的 Bad 会被算成 Late ——
                               直接污染我们的早晚偏差条和 anti-cheat 的 diff 数据。 */
                            judgements.push((Judgement::Bad, line_id, id, Some(t)));
                        }
                    }
                } else {
                    // flick
                    line.notes[id as usize].judge = JudgeStatus::PreJudge;
                    if let Some(tracker) = self.trackers.get_mut(&touch.id) {
                        tracker.flicked = false;
                    }
                }
            }
        }
        for _ in 0..keys_down {
            // find the earliest not judged click / hold note
            if let Some((line_id, id)) = chart
                .lines
                .iter()
                .zip(self.notes.iter())
                .enumerate()
                .filter_map(|(line_id, (line, (idx, st)))| {
                    idx[*st..]
                        .iter()
                        .cloned()
                        .find(|id| {
                            let note = &line.notes[*id as usize];
                            matches!(note.judge, JudgeStatus::NotJudged) && matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. })
                        })
                        .map(|id| (line_id, id))
                })
                .min_by_key(|(line_id, id)| chart.lines[*line_id].notes[*id as usize].time.not_nan())
            {
                let note = &mut chart.lines[line_id].notes[id as usize];
                let dt = (t - note.time).abs() / spd;
                if dt <= if matches!(note.kind, NoteKind::Click) { LIMIT_BAD } else { LIMIT_GOOD } {
                    match note.kind {
                        NoteKind::Click => {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((
                                if dt <= LIMIT_PERFECT {
                                    Judgement::Perfect
                                } else if dt <= LIMIT_GOOD {
                                    Judgement::Good
                                } else {
                                    Judgement::Bad
                                },
                                line_id,
                                id,
                                None,
                            ));
                        }
                        NoteKind::Hold { .. } => {
                            note.hitsound.play(res);
                            self.judgements.borrow_mut().push((t, line_id as _, id, Err(dt <= LIMIT_PERFECT)));
                            note.judge = JudgeStatus::Hold(dt <= LIMIT_PERFECT, t, t, false, f64::INFINITY);
                        }
                        _ => unreachable!(),
                    };
                }
            } else {
                break;
            }
        }
        for (line_id, ((line, pos), (idx, st))) in chart.lines.iter_mut().zip(pos.iter()).zip(self.notes.iter()).enumerate() {
            line.object.set_time(t);
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let NoteKind::Hold { end_time, .. } = &note.kind {
                    if let JudgeStatus::Hold(.., ref mut pre_judge, ref mut up_time) = note.judge {
                        if (*end_time - t) / spd <= LIMIT_BAD {
                            *pre_judge = true;
                            continue;
                        }
                        let x = &mut note.object.translation.0;
                        x.set_time(t);
                        let x = x.now();
                        if self.key_down_count == 0
                            && !pos
                                .iter()
                                .any(|it| it.is_some_and(|it| (it.x - x).abs() as f64 / note.judge_area as f64 <= X_DIFF_MAX))
                        {
                            if t > *up_time + UP_TOLERANCE {
                                note.judge = JudgeStatus::Judged;
                                judgements.push((Judgement::Miss, line_id, *id, None));
                            } else if up_time.is_infinite() {
                                *up_time = t;
                            }
                        } else {
                            *up_time = f64::INFINITY;
                        }
                        continue;
                    }
                }
                if !matches!(note.judge, JudgeStatus::NotJudged) {
                    continue;
                }
                // process miss
                let dt = (t - note.time) / spd;
                if dt > LIMIT_BAD {
                    note.judge = JudgeStatus::Judged;
                    judgements.push((Judgement::Miss, line_id, *id, None));
                    continue;
                }
                if -dt > LIMIT_BAD {
                    break;
                }
                if !matches!(note.kind, NoteKind::Drag) && (self.key_down_count == 0 || !matches!(note.kind, NoteKind::Flick)) {
                    continue;
                }
                let dt = dt.abs();
                let x = &mut note.object.translation.0;
                x.set_time(t);
                let x = x.now();
                if self.key_down_count != 0
                    || pos.iter().any(|it| {
                        it.is_some_and(|it| {
                            let dx = (it.x - x).abs() as f64 / note.judge_area as f64;
                            dx <= X_DIFF_MAX && dt <= (LIMIT_BAD - LIMIT_PERFECT * (dx - 0.9).max(0.))
                        })
                    })
                {
                    note.judge = JudgeStatus::PreJudge;
                }
            }
        }
        // process pre-judge
        for (line_id, (line, (idx, st))) in chart.lines.iter_mut().zip(self.notes.iter()).enumerate() {
            line.object.set_time(t);
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let JudgeStatus::Hold(perfect, .., diff, true, _) = note.judge {
                    if let NoteKind::Hold { end_time, .. } = &note.kind {
                        if *end_time <= t {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((if perfect { Judgement::Perfect } else { Judgement::Good }, line_id, *id, Some(diff)));
                            continue;
                        }
                    }
                }
                // TODO adjust
                let ghost_t = t + LIMIT_GOOD;
                if matches!(note.kind, NoteKind::Click) {
                    if ghost_t < note.time {
                        break;
                    }
                } else if t < note.time {
                    continue;
                }
                if matches!(note.judge, JudgeStatus::PreJudge) {
                    let diff = if let JudgeStatus::Hold(.., diff, _, _) = note.judge {
                        Some(diff)
                    } else {
                        None
                    };
                    note.judge = JudgeStatus::Judged;
                    if !matches!(note.kind, NoteKind::Click) {
                        judgements.push((Judgement::Perfect, line_id, *id, diff));
                    }
                }
            }
        }
        for (judgement, line_id, id, diff) in judgements {
            let line = &mut chart.lines[line_id];
            let note = &mut line.notes[id as usize];
            line.object.set_time(t);
            note.object.set_time(t);
            let line = &chart.lines[line_id];
            let note = &line.notes[id as usize];
            let line_tr = line.now_transform(res, &chart.lines);
            self.commit(
                t,
                judgement,
                line_id as _,
                id,
                if matches!(judgement, Judgement::Miss) {
                    0.25
                } else if matches!(note.kind, NoteKind::Drag | NoteKind::Flick) {
                    0.
                } else {
                    (diff.unwrap_or(t) - note.time) / spd
                },
            );
            if matches!(note.kind, NoteKind::Hold { .. }) {
                continue;
            }
            if match judgement {
                Judgement::Perfect => {
                    res.with_model(line_tr * note.object.now(res), |res| {
                        res.emit_at_origin(note.rotation(line), note.fx_color.unwrap_or_else(|| res.res_pack.info.fx_perfect()))
                    });
                    true
                }
                Judgement::Good => {
                    res.with_model(line_tr * note.object.now(res), |res| {
                        res.emit_at_origin(note.rotation(line), note.fx_color.unwrap_or_else(|| res.res_pack.info.fx_good()))
                    });
                    true
                }
                Judgement::Bad => {
                    if !matches!(note.kind, NoteKind::Hold { .. }) {
                        bad_notes.push(BadNote {
                            time: t,
                            kind: note.kind.clone(),
                            matrix: {
                                let mut mat = line_tr;
                                if !note.above {
                                    mat.append_nonuniform_scaling_mut(&Vector::new(1., -1.));
                                }
                                let incline_sin = line.incline.now_opt().map(|it| it.to_radians().sin()).unwrap_or_default();
                                mat *= note.now_transform(
                                    res,
                                    &line.ctrl_obj.borrow_mut(),
                                    ((note.height - line.height.now() as f64) / res.aspect_ratio as f64 * note.speed) as f32,
                                    incline_sin,
                                );
                                mat
                            },
                        });
                    }
                    false
                }
                _ => false,
            } {
                note.hitsound.play(res);
            }
        }
        for (line, (idx, st)) in chart.lines.iter().zip(self.notes.iter_mut()) {
            while idx
                .get(*st)
                .is_some_and(|id| matches!(line.notes[*id as usize].judge, JudgeStatus::Judged))
            {
                *st += 1;
            }
        }
        self.last_time = t / spd;
    }

    fn auto_play_update(&mut self, res: &mut Resource, chart: &mut Chart) {
        let t = res.time;
        let spd = res.config.speed as f64;
        let mut judgements = Vec::new();
        for (line_id, (line, (idx, st))) in chart.lines.iter_mut().zip(self.notes.iter_mut()).enumerate() {
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let JudgeStatus::Hold(..) = note.judge {
                    if let NoteKind::Hold { end_time, .. } = note.kind {
                        if t >= end_time {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((line_id, *id));
                            continue;
                        }
                    }
                }
                if !matches!(note.judge, JudgeStatus::NotJudged) {
                    continue;
                }
                if note.time > t {
                    break;
                }
                note.judge = if matches!(note.kind, NoteKind::Hold { .. }) {
                    note.hitsound.play(res);
                    self.judgements.borrow_mut().push((t, line_id as _, *id, Err(true)));
                    JudgeStatus::Hold(true, t, (t - note.time) / spd, false, f64::INFINITY)
                } else {
                    judgements.push((line_id, *id));
                    JudgeStatus::Judged
                };
            }
            while idx
                .get(*st)
                .is_some_and(|id| matches!(line.notes[*id as usize].judge, JudgeStatus::Judged))
            {
                *st += 1;
            }
        }
        for (line_id, id) in judgements.into_iter() {
            self.commit(t, Judgement::Perfect, line_id as _, id, 0.);
            let (note_transform, note_hitsound) = {
                let line = &mut chart.lines[line_id];
                let note = &mut line.notes[id as usize];
                let nt = if matches!(note.kind, NoteKind::Hold { .. }) { t } else { note.time };
                line.object.set_time(nt);
                note.object.set_time(nt);
                (note.object.now(res), note.hitsound.clone())
            };
            let line = &chart.lines[line_id];
            /* 【上游 PR #898】autoplay 也要尊重音符自己的 fx_color。
               原实现硬编码 fx_perfect()，自定义击中特效色的谱在 autoplay 下
               和普通游玩看到的颜色不一致 —— 谱师调的配色等于白调。 */
            let note = &line.notes[id as usize];
            res.with_model(line.now_transform(res, &chart.lines) * note_transform, |res| {
                res.emit_at_origin(note.rotation(line), note.fx_color.unwrap_or_else(|| res.res_pack.info.fx_perfect()))
            });
            if !matches!(note.kind, NoteKind::Hold { .. }) {
                note_hitsound.play(res);
            }
        }
    }

    #[inline]
    pub fn result(&self) -> PlayResult {
        self.inner.result()
    }

    #[inline]
    pub fn combo(&self) -> u32 {
        self.inner.combo()
    }

    #[inline]
    pub fn counts(&self) -> [u32; 4] {
        self.inner.counts()
    }

    /// 【话梅】早/晚计数，给实时判定条用。
    pub fn early_late_counts(&self) -> (u32, u32) {
        self.inner.early_late_counts()
    }
}

struct Handler {
    status: TouchStatus,
    wheel: (f32, f32),
}
impl Handler {
    fn finalize(&mut self) {
        if is_mouse_button_down(MouseButton::Left) {
            self.status.touches.push(Touch {
                id: button_to_id(MouseButton::Left),
                phase: TouchPhase::Moved,
                position: mouse_position().into(),
                time: f64::NEG_INFINITY,
            });
        }
    }
}

fn button_to_id(button: MouseButton) -> u64 {
    u64::MAX
        - match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Unknown => 3,
        }
}

impl EventHandler for Handler {
    fn update(&mut self, _: &mut miniquad::Context) {}
    fn draw(&mut self, _: &mut miniquad::Context) {}
    fn touch_event(&mut self, _: &mut miniquad::Context, phase: miniquad::TouchPhase, id: u64, x: f32, y: f32, time: f64) {
        self.status.touches.push(Touch {
            id,
            phase: phase.into(),
            position: vec2(x, y),
            time,
        });
    }

    fn mouse_wheel_event(&mut self, _ctx: &mut miniquad::Context, x: f32, y: f32) {
        self.wheel.0 += x;
        self.wheel.1 += y;
    }

    fn mouse_button_down_event(&mut self, _ctx: &mut miniquad::Context, button: MouseButton, x: f32, y: f32) {
        self.status.touches.push(Touch {
            id: button_to_id(button),
            phase: TouchPhase::Started,
            position: vec2(x, y),
            time: f64::NEG_INFINITY,
        });
    }

    fn mouse_button_up_event(&mut self, _ctx: &mut miniquad::Context, button: MouseButton, x: f32, y: f32) {
        self.status.touches.push(Touch {
            id: button_to_id(button),
            phase: TouchPhase::Ended,
            position: vec2(x, y),
            time: f64::NEG_INFINITY,
        });
    }

    fn key_down_event(&mut self, _ctx: &mut miniquad::Context, _keycode: KeyCode, _keymods: miniquad::KeyMods, repeat: bool) {
        if !repeat {
            self.status.key_delta += 1;
            self.status.keys_down += 1;
        }
    }

    fn key_up_event(&mut self, _ctx: &mut miniquad::Context, _keycode: KeyCode, _keymods: miniquad::KeyMods) {
        self.status.key_delta -= 1;
    }
}

#[derive(Default)]
pub struct PlayResult {
    pub score: u32,
    pub accuracy: f64,
    pub max_combo: u32,
    pub num_of_notes: u32,
    pub counts: [u32; 4],
    pub early: u32,
    pub late: u32,
    pub std: f32,
    pub early_kind: [u32; 4],
    pub late_kind: [u32; 4],
    /// 本局最大同时触点数（键盘模式为 0）。见 JudgeInner::max_touch。
    pub max_touch: u32,
}

pub fn icon_index(score: u32, full_combo: bool) -> usize {
    match (score, full_combo) {
        (x, _) if x < 700000 => 0,
        (x, _) if x < 820000 => 1,
        (x, _) if x < 880000 => 2,
        (x, _) if x < 920000 => 3,
        (x, _) if x < 960000 => 4,
        (1000000, _) => 7,
        (_, false) => 5,
        (_, true) => 6,
    }
}
/// 【话梅 2026-11-18】倒打/镜像的输入映射测试。
///
/// 这一层是整个移植里**唯一会直接毁掉手感**的地方，所以必须用断言锁死：
/// 写错一个轴，玩家点哪里都判不中，而且很难从现象反推原因。
#[cfg(test)]
mod flip_input_tests {
    use super::Judge;
    use crate::practice_view::PracticeView;

    const W: f32 = 1080.;

    fn map(flip_x: bool, flip_y: bool, x: f32, y: f32) -> (f32, f32) {
        Judge::chart_touch_math(flip_x, flip_y, PracticeView::default(), x, y, W)
    }

    /// 默认状态（不倒打、不镜像）必须**完全恒等** —— 这是改造前后不变的保证。
    #[test]
    fn default_is_identity() {
        assert_eq!(map(false, false, 0.3, -0.7), (0.3, -0.7));
        assert_eq!(map(false, false, -0.55, 0.25), (-0.55, 0.25));
        assert_eq!(map(false, false, 0., 0.), (0., 0.));
    }

    /// 只开水平镜像：x 取反，y 不动。
    #[test]
    fn flip_x_mirrors_x_only() {
        assert_eq!(map(true, false, 0.3, -0.7), (-0.3, -0.7));
    }

    /// 只开倒打（auto_flip_y）：**180° 旋转**，xy 同时取反。
    ///
    /// 注意这里传的是「已 XOR 过」的第一参数（false ^ true = true），
    /// 与调用处的写法一致。
    #[test]
    fn auto_flip_rotates_180() {
        assert_eq!(map(true, true, 0.3, -0.7), (-0.3, 0.7));
    }

    /// 水平镜像 + 倒打 = 垂直镜像（x 还原，y 取反）。
    /// 这条验证的是 `chart_flip = (u^a)^a = u` 那个化简。
    #[test]
    fn flip_x_plus_auto_flip_equals_flip_y() {
        // u=true, a=true → 第一参数 = true^true = false
        assert_eq!(map(false, true, 0.3, -0.7), (0.3, 0.7));
    }

    /// 缩放视野：scale=200% 时坐标应放大两倍。
    #[test]
    fn practice_view_scale_applies() {
        let v = PracticeView {
            scale_percent: 200.,
            center_x: 0.,
            center_y: 0.,
        };
        let (x, y) = Judge::chart_touch_math(false, false, v, 0.4, -0.2, W);
        assert!((x - 0.2).abs() < 1e-6, "x={x}");
        assert!((y - -0.1).abs() < 1e-6, "y={y}");
    }
}
