#![allow(unused)]

prpr_l10n::tl_file!("game");

use super::{
    draw_background,
    ending::RecordUpdateState,
    loading::{BasicPlayer, SaveFn, UpdateFn, UploadFn},
    request_input, return_input, show_message, take_input, EndingScene, NextScene, Scene,
};
use crate::{
    bin::BinaryReader,
    config::{Config, Mods},
    core::{copy_fbo, BadNote, Chart, ChartExtra, Effect, Point, Resource, UIElement, Vector, PGR_FONT},
    ext::{parse_time, screen_aspect, semi_white, RectExt, SafeTexture, ScaleType},
    fs::FileSystem,
    info::{ChartFormat, ChartInfo},
    judge::Judge,
    parse::{parse_extra, parse_pec, parse_phigros, parse_rpe},
    task::Task,
    time::TimeManager,
    ui::{OffsetAnalysisPanel, OffsetPanelAction, OffsetPanelLabels, RectButton, TextPainter, Ui},
};
use anyhow::{bail, Context, Result};
use concat_string::concat_string;
use inputbox::InputBox;
use macroquad::{prelude::*, window::InternalGlContext};
use sasa::{Music, MusicParams};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    cell::RefCell,
    fs::File,
    io::{Cursor, ErrorKind},
    ops::{Deref, DerefMut, Range},
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use tracing::{debug, warn};

const PAUSE_CLICK_INTERVAL: f32 = 0.7;

#[rustfmt::skip]
#[cfg(closed)]
mod inner;
#[cfg(closed)]
use inner::*;

const WAIT_TIME: f64 = 0.5;
const AFTER_TIME: f64 = 0.7;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimpleRecord {
    pub score: i32,
    pub accuracy: f32,
    pub full_combo: bool,
}

impl SimpleRecord {
    pub fn update(&mut self, other: &SimpleRecord) -> bool {
        let mut changed = false;
        if other.score > self.score {
            self.score = other.score;
            changed = true;
        }
        if other.accuracy > self.accuracy {
            self.accuracy = other.accuracy;
            changed = true;
        }
        if other.full_combo & !self.full_combo {
            self.full_combo = other.full_combo;
            changed = true;
        }
        changed
    }
}

fn fmt_time(t: f32) -> String {
    let f = t < 0.;
    let t = t.abs();
    let secs = t % 60.;
    let mut t = (t / 60.) as u64;
    let mins = t % 60;
    t /= 60;
    let hrs = t % 100;
    format!("{}{hrs:02}:{mins:02}:{secs:05.2}", if f { "-" } else { "" })
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    fn on_game_start();
}

#[derive(PartialEq, Eq)]
pub enum GameMode {
    Normal,
    TweakOffset,
    Exercise,
    NoRetry,
    View,
}

#[derive(Clone)]
enum State {
    Starting,
    BeforeMusic,
    Playing,
    Ending,
}

pub struct GameScene {
    /* ── 计时完整性（反作弊 + 数据质量）──
       音游最常见的作弊是改时间：把歌快进跑完。它会让「游玩中的真实耗时」
       和「谱面时长」对不上 —— 这是唯一一个**不依赖客户端自报秒数**的指标。 */
    /// 游玩中且未暂停的累计真实耗时（秒）
    play_wall: f64,
    /// 暂停次数
    pause_count: u16,
    /// 帧间隔 > 100ms 的次数（卡顿帧，AI 训练要降权）
    dt_outliers: u16,
    /// 上一帧是否处于暂停，用来数「暂停了几次」
    was_paused: bool,

    /// 【话梅】实时判定条状态
    timing_bar: crate::timing_bar::TimingBar,
    /// 【话梅】TAA 状态（历史纹理 + 抖动计数）
    taa: crate::taa::Taa,

    should_exit: bool,
    next_scene: Option<NextScene>,

    pub mode: GameMode,
    pub res: Resource,
    pub chart: Chart,
    pub judge: Judge,
    pub gl: InternalGlContext<'static>,
    player: Option<BasicPlayer>,
    chart_bytes: Vec<u8>,
    chart_format: ChartFormat,
    info_offset: f32,
    effects: Vec<Effect>,
    offset_analysis: OffsetAnalysisPanel,

    first_in: bool,
    exercise_range: Range<f64>,
    exercise_press: Option<(i8, u64)>,
    exercise_btns: (RectButton, RectButton),

    pub music: Music,

    state: State,
    pub last_update_time: f64,
    pause_rewind: Option<f64>,
    pause_first_time: f32,

    pub bad_notes: Vec<BadNote>,

    upload_fn: Option<UploadFn>,
    update_fn: Option<UpdateFn>,
    save_fn: Option<SaveFn>,

    best_record: Option<SimpleRecord>,

    pub touch_points: Vec<(f32, f32)>,
    fps_frame_count: u32,
    fps_total_time: f64,
    fps_last_frame_time: f64,

    dead: bool,
}

macro_rules! reset {
    ($self:ident, $res:expr, $tm:ident) => {{
        $self.bad_notes.clear();
        $self.judge.reset();
        $self.timing_bar.clear();
        $self.chart.reset();
        $res.judge_line_color = $res.res_pack.info.color_perfect();
        $self.music.pause()?;
        $self.music.seek_to(0.)?;
        $tm.speed = $res.config.speed as _;
        $tm.reset();
        $self.last_update_time = $tm.now();
        $self.state = State::Starting;
        /* 陀螺仪采样器清零并开始累积。
           ⚠ 必须在这里（而不是结算时）——否则 Sampler::running 一直是 false，
             每帧样本都被 push() 丢掉，结算永远是 None。见 huamei.rs 的注释。 */
        crate::huamei::begin_round_motion();
        $self.fps_frame_count = 0;
        $self.fps_total_time = 0.0;
        $self.fps_last_frame_time = $tm.real_time();
        $self.dead = false;
    }};
}

impl GameScene {
    pub const BEFORE_TIME: f64 = 0.7;
    pub const FADEOUT_TIME: f64 = WAIT_TIME + AFTER_TIME + 0.3;

    pub async fn load_chart_bytes(fs: &mut dyn FileSystem, info: &ChartInfo) -> Result<Vec<u8>> {
        if let Ok(bytes) = fs.load_file(&info.chart).await {
            return Ok(bytes);
        }
        if let Some(name) = info.chart.strip_suffix(".pec") {
            if let Ok(bytes) = fs.load_file(&concat_string!(name, ".json")).await {
                return Ok(bytes);
            }
        }
        bail!("Cannot find chart file")
    }

    pub fn infer_chart_format(info: &ChartInfo, bytes: &[u8]) -> ChartFormat {
        info.format.clone().unwrap_or_else(|| {
            if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                if text.starts_with('{') {
                    if text.contains("\"META\"") {
                        ChartFormat::Rpe
                    } else {
                        ChartFormat::Pgr
                    }
                } else {
                    ChartFormat::Pec
                }
            } else {
                ChartFormat::Pbc
            }
        })
    }

    pub async fn load_chart(fs: &mut dyn FileSystem, info: &ChartInfo) -> Result<(Chart, Vec<u8>, ChartFormat)> {
        let extra = fs.load_file("extra.json").await.ok().map(String::from_utf8).transpose()?;
        let extra = if let Some(extra) = extra {
            parse_extra(&extra, fs).await.context("Failed to parse extra")?
        } else {
            ChartExtra::default()
        };
        let bytes = Self::load_chart_bytes(fs, info).await.context("Failed to load chart")?;
        let format = Self::infer_chart_format(info, &bytes);
        let mut chart = match format {
            ChartFormat::Rpe => parse_rpe(&String::from_utf8_lossy(&bytes), fs, extra, info.use_rpe_170_speed.unwrap_or_default()).await,
            ChartFormat::Pgr => parse_phigros(&String::from_utf8_lossy(&bytes), extra),
            ChartFormat::Pec => parse_pec(&String::from_utf8_lossy(&bytes), extra),
            ChartFormat::Pbc => {
                let mut r = BinaryReader::new(Cursor::new(&bytes));
                r.read()
            }
        }?;
        /* ⚠ 加 context：这里原来直接 `?`，缺文件时只剩一句 "os error 2"，
           完全看不出缺的是谱面引用的哪张图。2026-11-16 卡了一整轮。 */
        chart.load_textures(fs).await.context("load chart textures")?;
        chart.settings.hold_partial_cover = info.hold_partial_cover;
        Ok((chart, bytes, format))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        mode: GameMode,
        info: ChartInfo,
        mut config: Config,
        mut fs: Box<dyn FileSystem>,
        player: Option<BasicPlayer>,
        background: SafeTexture,
        illustration: SafeTexture,
        upload_fn: Option<UploadFn>,
        update_fn: Option<UpdateFn>,
        save_fn: Option<SaveFn>,
    ) -> Result<Self> {
        match mode {
            GameMode::TweakOffset => {
                config.mods.insert(Mods::AUTOPLAY);
            }
            GameMode::Exercise => {
                config.mods.remove(Mods::AUTOPLAY);
            }
            _ => {}
        }

        let (mut chart, chart_bytes, chart_format) = Self::load_chart(fs.deref_mut(), &info).await?;
        if config.mods.contains(Mods::NO_SHADER) {
            chart.extra.effects.clear();
            chart.extra.global_effects.clear();
        }
        let effects = std::mem::take(&mut chart.extra.global_effects);
        if config.fxaa {
            chart
                .extra
                .effects
                .push(Effect::new(0.0..f64::INFINITY, include_str!("fxaa.glsl"), Vec::new(), false).unwrap());
        }

        if config.has_mod(Mods::NIGHTCORE) {
            config.speed *= 1.5;
        }

        if config.has_mod(Mods::RAINBOW) {
            chart
                .extra
                .effects
                .push(Effect::new(0.0..f64::INFINITY, include_str!("rainbow.glsl"), Vec::new(), false).unwrap());
        }

        let info_offset = info.offset;
        let mut res = Resource::new(
            config,
            info,
            fs,
            player.as_ref().and_then(|it| it.avatar.clone()),
            background,
            illustration,
            chart.extra.effects.is_empty() && effects.is_empty(),
        )
        .await
        .context("Failed to load resources")?;

        // Prepare extra sfx from chart.hitsounds
        chart.hitsounds.drain().for_each(|(name, clip)| {
            if let Ok(clip) = res.create_sfx(clip) {
                res.extra_sfxs.insert(name, clip);
            }
        });

        let exercise_range = (chart.offset + info_offset + res.config.offset) as f64..res.track_length;

        let judge = Judge::new(&chart);

        let music = Self::new_music(&mut res)?;
        Ok(Self {
            should_exit: false,
            next_scene: None,

            mode,
            res,
            chart,
            judge,
            gl: unsafe { get_internal_gl() },
            player,
            chart_bytes,
            chart_format,
            effects,
            info_offset,

            offset_analysis: OffsetAnalysisPanel::new(),

            first_in: false,
            exercise_range,
            exercise_press: None,
            exercise_btns: (RectButton::new(), RectButton::new()),

            music,

            state: State::Starting,
            play_wall: 0.,
            pause_count: 0,
            dt_outliers: 0,
            was_paused: false,
            timing_bar: crate::timing_bar::TimingBar::default(),
            taa: crate::taa::Taa::default(),
            last_update_time: 0.,
            pause_rewind: None,
            pause_first_time: f32::NEG_INFINITY,

            bad_notes: Vec::new(),

            upload_fn,
            update_fn,
            save_fn,

            best_record: None,

            touch_points: Vec::new(),

            fps_frame_count: 0,
            fps_total_time: 0.0,
            fps_last_frame_time: 0.0,

            dead: false,
        })
    }

    fn new_music(res: &mut Resource) -> Result<Music> {
        res.audio.create_music(
            res.music.clone(),
            MusicParams {
                amplifier: res.config.volume_music as _,
                playback_rate: res.config.speed as _,
                ..Default::default()
            },
        )
    }

    fn touch_scale(&self) -> f32 {
        (screen_width() / screen_height()) / self.res.aspect_ratio
    }

    fn ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        let time = tm.now();
        let p = match self.state {
            State::Starting => {
                if time <= Self::BEFORE_TIME {
                    1. - (1. - time / Self::BEFORE_TIME).powi(3)
                } else {
                    1.
                }
            }
            State::BeforeMusic => 1.,
            State::Playing => 1.,
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                1. - (t / (AFTER_TIME + 0.3)).min(1.).powi(2)
            }
        } as f32;
        let res = &mut self.res;
        let eps = 2e-2 / res.aspect_ratio;
        let top = -1. / res.aspect_ratio;
        let pause_w = 0.015;
        let pause_h = pause_w * 3.2;
        let pause_center = Point::new(pause_w * 4.0 - 1., top + eps * 3.5 - (1. - p) * 0.4 + pause_h / 2.);
        if res.config.interactive
            && !tm.paused()
            && self.pause_rewind.is_none()
            && Judge::get_touches().iter().any(|touch| {
                touch.phase == TouchPhase::Started && {
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    (pause_center - p).norm() < 0.05
                }
            })
        {
            let t = tm.now() as f32;
            if t - self.pause_first_time > PAUSE_CLICK_INTERVAL && res.config.double_click_to_pause {
                self.pause_first_time = t;
            } else {
                self.pause_first_time = f32::NEG_INFINITY;
                if !self.music.paused() {
                    self.music.pause()?;
                }
                tm.pause();
                #[cfg(target_env = "ohos")]
                miniquad::native::set_interceptor_state(false);
            }
        }
        ui.alpha(res.alpha, |ui| {
            ui.text("MAGIC BUGFIX TEXT").color(Color::new(0., 0., 0., 0.)).draw();
            if tm.now() as f32 - self.pause_first_time <= PAUSE_CLICK_INTERVAL {
                ui.fill_circle(pause_center.x, pause_center.y, 0.05, Color::new(1., 1., 1., 0.5));
            }

            let margin = 0.03;

            let legacy_aui = !res.info.use_attach_ui_fix.unwrap_or_default();
            let unit_h = if legacy_aui { ui.text("0").measure_using(&PGR_FONT).h } else { 0. };

            // score
            let h = 0.07;
            let score_top = top + eps * 2.2 - (1. - p) * 0.4;
            let score_right = 1. - margin;
            let score = format!("{:07}", self.judge.score());
            let scale_point = legacy_aui.then(|| {
                let ct = ui.text(&score).size(0.8).measure_using(&PGR_FONT).center();
                (score_right - ct.x, score_top + ct.y)
            });
            self.chart
                .with_element(ui, res, UIElement::Score, scale_point, (score_right, score_top), |ui, c| {
                    ui.text(&score)
                        .pos(score_right, score_top)
                        .anchor(1., 0.)
                        .size(0.8)
                        .color(c)
                        .draw_using(&PGR_FONT);
                    if res.config.show_acc {
                        ui.text(format!("{:05.2}%", self.judge.real_time_accuracy() * 100.))
                            .pos(1. - margin, score_top + h)
                            .anchor(1., 0.)
                            .size(0.4)
                            .color(Color { a: c.a * 0.7, ..c })
                            .draw_using(&PGR_FONT);
                    }
                });

            self.chart.with_element(
                ui,
                res,
                UIElement::Pause,
                legacy_aui.then(|| (pause_center.x, pause_center.y)),
                (pause_center.x - pause_w * 1.5, pause_center.y - pause_h / 2.),
                |ui, c| {
                    let mut r = Rect::new(pause_center.x - pause_w * 1.5, pause_center.y - pause_h / 2., pause_w, pause_h);
                    ui.fill_rect(r, c);
                    r.x += pause_w * 2.;
                    ui.fill_rect(r, c);
                },
            );
            if self.judge.combo() >= 3 {
                if legacy_aui {
                    let combo_top = top + eps * 2. - (1. - p) * 0.4;
                    let btm = self
                        .chart
                        .with_element(ui, res, UIElement::ComboNumber, None, (0., combo_top + unit_h / 2.), |ui, c| {
                            ui.text(self.judge.combo().to_string())
                                .pos(0., combo_top)
                                .anchor(0.5, 0.)
                                .color(c)
                                .draw_using(&PGR_FONT)
                                .bottom()
                        });
                    let combo_top = btm + 0.01;
                    self.chart
                        .with_element(ui, res, UIElement::Combo, None, (0., combo_top + unit_h * 0.2), |ui, c| {
                            ui.text(if res.config.autoplay() { "AUTOPLAY" } else { "COMBO" })
                                .pos(0., combo_top)
                                .anchor(0.5, 0.)
                                .size(0.4)
                                .color(c)
                                .draw_using(&PGR_FONT);
                        });
                } else {
                    let combo = self.judge.combo().to_string();
                    let ct = ui.text(&combo).size(1.0).measure().center();
                    let combo_y = top + eps * 2. - (1. - p) * 0.4 + ct.y;
                    let btm = self.chart.with_element(ui, res, UIElement::ComboNumber, None, (0., combo_y), |ui, c| {
                        ui.text(&combo)
                            .pos(0., combo_y)
                            .anchor(0.5, 0.5)
                            .size(1.0)
                            .color(c)
                            .draw_using(&PGR_FONT)
                            .bottom()
                    });
                    let ct = ui.text("COMBO").size(0.4).measure().center();
                    let combo_top = btm + 0.01 + ct.y;
                    self.chart.with_element(ui, res, UIElement::Combo, None, (0., combo_top), |ui, c| {
                        ui.text(if res.config.autoplay() { "AUTOPLAY" } else { "COMBO" })
                            .pos(0., combo_top)
                            .anchor(0.5, 0.5)
                            .size(0.4)
                            .color(c)
                            .draw_using(&PGR_FONT);
                    });
                }
            }
            // magic to make score visible, refer to phira/src/rate.rs#L219
            ui.text("").draw_using(&PGR_FONT);
            let lf = -1. + margin;
            let bt = -top - eps * 2.8 + (1. - p) * 0.4;
            let scale_point = legacy_aui.then(|| {
                let ct = ui.text(&res.info.name).size(0.5).measure().center();
                (lf + ct.x, bt - ct.y)
            });
            self.chart.with_element(ui, res, UIElement::Name, scale_point, (lf, bt), |ui, c| {
                ui.text(&res.info.name)
                    .pos(lf, bt)
                    .anchor(0., 1.)
                    .size(0.5)
                    .color(c)
                    .max_width(0.8)
                    .draw();
            });

            let scale_point = legacy_aui.then(|| {
                let ct = ui.text(&res.info.level).size(0.5).measure().center();
                (-lf - ct.x, bt - ct.y)
            });
            self.chart.with_element(ui, res, UIElement::Level, scale_point, (-lf, bt), |ui, c| {
                ui.text(&res.info.level).pos(-lf, bt).anchor(1., 1.).size(0.5).color(c).draw();
            });

            let hw = 0.003;
            let height = eps * 1.0;
            let dest = (2. * res.time / res.track_length).clamp(0., 2.) as f32;
            self.chart
                .with_element(ui, res, UIElement::Bar, Some((-1., top + height / 2.)), (-1., top + height / 2.), |ui, color| {
                    ui.fill_rect(Rect::new(-1., top, dest, height), semi_white(0.6));
                    ui.fill_rect(Rect::new(-1. + dest - hw, top, hw * 2., height), WHITE);
                });
        });
        Ok(())
    }

    fn overlay_ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        let c = semi_white(self.res.alpha);
        let res = &mut self.res;
        if tm.paused() {
            let h = 1. / res.aspect_ratio;
            draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., 0.6));
            let o = if self.mode == GameMode::Exercise { -0.3 } else { 0. };
            let s = 0.06;
            let w = 0.05;
            let no_retry = self.mode == GameMode::NoRetry;
            draw_texture_ex(
                *res.icon_back,
                -s * 3. - w,
                -s + o,
                c,
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            let r = Rect::new(0., o, 0., 0.).feather(s);
            let disabled_color = semi_white(res.alpha * 0.4);
            ui.fill_rect(r, (*res.icon_retry, r.feather(0.02), ScaleType::Fit, if no_retry { disabled_color } else { c }));
            draw_texture_ex(
                *res.icon_resume,
                s + w,
                -s + o,
                if self.dead { disabled_color } else { c },
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            if res.config.interactive {
                let mut clicked = None;
                for touch in Judge::get_touches() {
                    if touch.phase != TouchPhase::Started {
                        continue;
                    }
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    for i in -1..=1 {
                        let ct = Point::new((s * 2. + w) * i as f32, o);
                        let d = p - ct;
                        if d.x.abs() <= s && d.y.abs() <= s {
                            clicked = Some(i);
                            break;
                        }
                    }
                }
                if no_retry && clicked == Some(0) || self.dead && clicked == Some(1) {
                    clicked = None;
                }
                let mut pos = self.music.position();
                if self.mode == GameMode::Exercise {
                    pos = tm.now();
                }
                if clicked.is_some_and(|it| it != -1) && (tm.speed - res.config.speed as f64).abs() > 0.01 {
                    debug!("recreating music");
                    self.music = res.audio.create_music(
                        res.music.clone(),
                        MusicParams {
                            amplifier: res.config.volume_music as _,
                            playback_rate: res.config.speed as _,
                            ..Default::default()
                        },
                    )?;
                }
                match clicked {
                    Some(-1) => {
                        self.should_exit = true;
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(false);
                    }
                    Some(0) => {
                        reset!(self, res, tm);
                        if self.mode == GameMode::Exercise {
                            self.judge.advance_to(&mut self.chart, self.exercise_range.start);
                        }
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(true);
                    }
                    Some(1) => {
                        if self.mode == GameMode::Exercise && (tm.now() > self.exercise_range.end || tm.now() < self.exercise_range.start) {
                            tm.seek_to(self.exercise_range.start);
                            self.music.seek_to(self.exercise_range.start)?;
                            pos = self.exercise_range.start;
                        }
                        self.music.play()?;
                        res.time -= 3.;
                        let dst = pos - 3.;
                        if dst < 0. {
                            self.music.pause()?;
                            self.state = State::BeforeMusic;
                        } else {
                            self.music.seek_to(dst)?;
                        }
                        let now = tm.now();
                        tm.speed = res.config.speed as _;
                        tm.resume();
                        tm.seek_to(now - 3.);
                        self.pause_rewind = Some(tm.now() - 0.2);
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(true);
                    }
                    _ => {}
                }
            }
            if self.mode == GameMode::Exercise {
                let asp = self.touch_scale();
                for touch in ui.ensure_touches() {
                    touch.position *= asp;
                }
                ui.scope(|ui| {
                    ui.dx(0.3);
                    ui.dy(-0.3);
                    ui.slider(tl!("speed"), 0.5..2.0, 0.05, &mut self.res.config.speed, Some(0.5));
                });
                ui.dy(0.06);
                let hw = 0.7;
                let h = 0.06;
                let eh = 0.12;
                let rad = 0.03;
                let sp = self.offset().min(0.) as f64;
                ui.fill_rect(Rect::new(-hw, -h, hw * 2., h * 2.), GRAY);
                let st = -hw + ((self.exercise_range.start - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                let en = -hw + ((self.exercise_range.end - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                let t = tm.now();
                let cur = -hw + ((t - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                ui.fill_rect(Rect::new(st, -h, en - st, h * 2.), WHITE);
                ui.fill_rect(Rect::new(st, -eh, 0., eh + h).feather(0.005), BLUE);
                ui.fill_circle(st, -eh, rad, BLUE);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(st, -eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (-1, it.id));
                }
                ui.fill_rect(Rect::new(en, -h, 0., eh + h).feather(0.005), RED);
                ui.fill_circle(en, eh, rad, RED);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(en, eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (1, it.id));
                }
                ui.fill_rect(Rect::new(cur, -h, 0., h * 2.).feather(0.005), GREEN);
                ui.fill_circle(cur, 0., rad, GREEN);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(cur, 0., 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (0, it.id));
                }
                ui.text(fmt_time(t as f32)).pos(0., -0.23).anchor(0.5, 0.).size(0.8).draw();
                if let Some((ctrl, id)) = &self.exercise_press {
                    if let Some(touch) = Judge::get_touches().iter().rfind(|it| it.id == *id) {
                        let x = touch.position.x;
                        let p = (x + hw) as f64 / (hw * 2.) as f64 * (self.res.track_length - sp) + sp;
                        let p = if self.res.track_length - sp <= 3. || *ctrl == 0 {
                            p.clamp(sp, self.res.track_length)
                        } else {
                            p.clamp(
                                if *ctrl == -1 { sp } else { self.exercise_range.start + 3. },
                                if *ctrl == -1 {
                                    self.exercise_range.end - 3.
                                } else {
                                    self.res.track_length
                                },
                            )
                        };
                        if *ctrl == 0 {
                            tm.seek_to(p);
                            self.music.seek_to(p)?;
                            self.bad_notes.clear();
                            self.judge.reset();
                            self.chart.reset();
                            self.res.judge_line_color = self.res.res_pack.info.color_perfect();
                        } else {
                            *(if *ctrl == -1 {
                                &mut self.exercise_range.start
                            } else {
                                &mut self.exercise_range.end
                            }) = p;
                        }
                        if matches!(touch.phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                            self.exercise_press = None;
                        }
                    }
                }
                ui.dy(0.2);
                let r = ui.text(tl!("to")).size(0.8).anchor(0.5, 0.).draw();
                let mut tx = ui
                    .text(fmt_time(self.exercise_range.start as f32))
                    .pos(r.x - 0.02, 0.)
                    .anchor(1., 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure();
                self.exercise_btns.0.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.0.touching() { 0.5 } else { 1. }));
                tx.draw();

                let mut tx = ui
                    .text(fmt_time(self.exercise_range.end as f32))
                    .pos(r.right() + 0.02, 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure();
                self.exercise_btns.1.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.1.touching() { 0.5 } else { 1. }));
                tx.draw();
                for touch in ui.ensure_touches() {
                    touch.position /= asp;
                }
            }
        }
        if let Some(time) = self.pause_rewind {
            let dt = tm.now() - time;
            let t = 3 - dt.floor() as i32;
            if t <= 0 {
                self.pause_rewind = None;
            } else {
                let a = (1. - dt as f32 / 3.) * 1.;
                let h = 1. / self.res.aspect_ratio;
                draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., a));
                ui.text(t.to_string()).anchor(0.5, 0.5).size(1.).color(c).draw();
            }
        }
        if self.res.config.touch_debug {
            for touch in Judge::get_touches() {
                ui.fill_circle(touch.position.x, touch.position.y, 0.04, Color { a: 0.4, ..RED });
            }
        }
        for pos in &self.touch_points {
            ui.fill_circle(pos.0, pos.1, 0.04, Color { a: 0.4, ..BLUE });
        }
        Ok(())
    }

    fn interactive(res: &Resource, state: &State) -> bool {
        res.config.interactive && matches!(state, State::Playing)
    }

    fn offset(&self) -> f32 {
        self.chart.offset + self.res.config.offset + self.info_offset
    }

    fn tweak_offset(&mut self, ui: &mut Ui, ita: bool) {
        let labels = OffsetPanelLabels {
            adjust_offset: tl!("adjust-offset"),
            auto_offset: tl!("auto-offset-btn"),
            analysis_prompt: tl!("analysis-prompt"),
            analysis_computing: tl!("analysis-computing"),
            cancel: tl!("offset-cancel"),
            reset: tl!("offset-reset"),
            save: tl!("offset-save"),
        };
        match self.offset_analysis.render(ui, &self.chart, &mut self.info_offset, ita, &labels) {
            Some(OffsetPanelAction::Cancel) => self.next_scene = Some(NextScene::PopWithResult(Box::new(None::<f32>))),
            Some(OffsetPanelAction::Reset) => self.info_offset = 0.,
            Some(OffsetPanelAction::Save(offset)) => self.next_scene = Some(NextScene::PopWithResult(Box::new(Some(offset)))),
            None => {}
        }
    }
    pub fn get_avg_fps(&self) -> Option<f32> {
        if self.fps_frame_count > 0 && self.fps_total_time > 0.0 {
            Some(self.fps_frame_count as f32 / self.fps_total_time as f32)
        } else {
            None
        }
    }
}

impl Scene for GameScene {
    fn enter(&mut self, tm: &mut TimeManager, target: Option<RenderTarget>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        on_game_start();
        #[cfg(target_env = "ohos")]
        miniquad::native::set_interceptor_state(true);
        self.music = Self::new_music(&mut self.res)?;
        self.res.camera.render_target = target;
        tm.speed = self.res.config.speed as _;
        tm.adjust_time = self.res.config.adjust_time;
        reset!(self, self.res, tm);
        set_camera(&self.res.camera);
        self.first_in = true;
        Ok(())
    }

    /// 对局自己走 chart_target 的 TAA/MSAA，全局那一层要让位。
    fn handles_own_aa(&self) -> bool {
        true
    }

    fn pause(&mut self, tm: &mut TimeManager) -> Result<()> {
        if !tm.paused() {
            self.pause_rewind = None;
            self.music.pause()?;
            tm.pause();
        }
        #[cfg(target_env = "ohos")]
        miniquad::native::set_interceptor_state(false);
        Ok(())
    }

    fn resume(&mut self, tm: &mut TimeManager) -> Result<()> {
        if !matches!(self.state, State::Playing) {
            tm.resume();
        }
        Ok(())
    }

    fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        /* 陀螺仪采一帧。**必须在游戏循环里**而不是结算时读一次：
           我们要的是「这一局打的过程中手稳不稳」，不是「结束时那一瞬间」。
           未注册实现时是空操作（非 iOS / 采样器启动失败）。 */
        crate::huamei::sample_motion();

        /* 触控落点：只记**按下那一刻**的坐标。
           ⚠ 不能记每一帧的触点 —— 按住不动的手指会每帧都被算一次，
             600 个音符的谱会出现几万次重复计数，热度图彻底失真。
           ⚠ 坐标系是 -1..1（prpr 的 UI 空间），转成 0..1 再交给热度图。 */
        for t in crate::judge::Judge::get_touches() {
            if matches!(t.phase, TouchPhase::Started) {
                crate::huamei::record_touch((t.position.x + 1.) / 2., (t.position.y + 1.) / 2.);
            }
        }
        self.offset_analysis
            .update(&self.chart, &self.res, self.info_offset, tm.real_time() as f32);

        self.res.audio.recover_if_needed()?;
        if matches!(self.state, State::Playing) {
            tm.update(self.music.position());
        }
        if self.mode == GameMode::Exercise && tm.now() > self.exercise_range.end && !tm.paused() {
            let state = self.state.clone();
            reset!(self, self.res, tm);
            self.state = state;
            tm.seek_to(self.exercise_range.start);
            tm.pause();
            self.music.pause()?;
            #[cfg(target_env = "ohos")]
            miniquad::native::set_interceptor_state(false);
        }
        let offset = self.offset();
        let time = tm.now();
        let time = match self.state {
            State::Starting => {
                if time >= Self::BEFORE_TIME {
                    self.res.alpha = 1.;
                    self.state = State::BeforeMusic;
                    tm.reset();
                    tm.seek_to(if self.mode == GameMode::Exercise {
                        self.exercise_range.start
                    } else {
                        offset.min(0.) as f64
                    });
                    self.last_update_time = tm.real_time();
                    if self.first_in && self.mode == GameMode::Exercise {
                        tm.pause();
                        self.first_in = false;
                    }
                    tm.now()
                } else {
                    #[cfg(target_os = "windows")]
                    {
                        // wtf bro. why must particles exist on Windows?
                        let emitter_config = self.res.emitter.emitter.config.clone();
                        let emitter_square_config = self.res.emitter.emitter_square.config.clone();
                        self.res.emitter.emitter.config.size = 0.0;
                        self.res.emitter.emitter_square.config.size = 0.0;
                        self.res.emitter.emitter.emit(vec2(0.0, 0.0), 1);
                        self.res.emitter.emitter_square.emit(vec2(0.0, 0.0), 1);
                        self.res.emitter.emitter.config = emitter_config;
                        self.res.emitter.emitter_square.config = emitter_square_config;
                    }
                    self.res.alpha = (1. - (1. - time / Self::BEFORE_TIME).powi(3)) as f32;
                    if self.mode == GameMode::Exercise {
                        self.exercise_range.start
                    } else {
                        offset as f64
                    }
                }
            }
            State::BeforeMusic => {
                if time >= 0.0 {
                    self.music.seek_to(time)?;
                    if !tm.paused() {
                        self.music.play()?;
                    }
                    self.state = State::Playing;
                }
                time
            }
            State::Playing => {
                if time > self.res.track_length + WAIT_TIME {
                    self.state = State::Ending;
                    #[cfg(target_env = "ohos")]
                    miniquad::native::set_interceptor_state(false);
                }
                time
            }
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                if t >= AFTER_TIME + 0.3 {
                    let result = self.judge.result();
                    let mut record_data = None;

                    // ══════════════════════════════════════════════════════
                    // 【huamei 改动】数据飞轮的上报载荷
                    // ══════════════════════════════════════════════════════
                    // 上游把整段上报包在 `#[cfg(closed)]` 里 —— 闭源构建才有的
                    // `encode_record` + 服务端 `/play/upload`。开源构建里
                    // `rated` 恒为 false，从来不上传（`song.rs:861`）。
                    //
                    // 我们不要官方那套加密 blob，只要**逐音符判定**。
                    // 管道原样复用：`record_data` → `EndingScene` → `upload_fn`，
                    // 我们只负责把载荷换掉。所以这里也**不再需要 `upload_fn` 非空**
                    // ——那是官方的发送器；我们的发送器在 phira 侧。
                    //
                    // ⚠ 与上游守卫的两点差别：
                    //   · 去掉 `player` 要求 —— huamei 只登自己的账号，
                    //     没有 Phira player id（身份由 HTTP token 承载）
                    //   · 去掉 `use_keyboard` 拒绝 —— 桌面端键盘是正当玩法，
                    //     改成打标记上传，由服务端分层
                    //   保留 `offline_mode` / `UNRATED` / `speed >= 1.0`：
                    //   变速或带非评级 MOD 的判定数据不可比，收了会污染数据集。
                    if !self.res.config.offline_mode
                        && !self.res.config.mods.intersects(Mods::UNRATED)
                        && self.res.config.speed >= 1.0 - 1e-3
                    {
                        if let Some(chart_id) = self.res.info.id {
                            // level 是展示串（形如 "IN  Lv.14.3"），取首个词当难度标签
                            let level = self.res.info.level.split_whitespace().next().unwrap_or("").to_owned();
                            /* 【话梅】记录这一局，供本地 RKS（R10+B20）计算。
                               放在这里而不是 ending.rs：此处同时握有 chart_id、
                               完整 result 与难度标签，不需要再穿参。
                               注意**不受下面 upload 条件的影响** —— 用户关掉上报
                               只是不上传，本地成绩该记还是记。 */
                            crate::huamei::record_score(
                                chart_id,
                                &level,
                                result.accuracy as f32,
                                result.score as i32,
                                chrono::Utc::now().timestamp_millis(),
                            );
                            let payload = crate::huamei::build_payload(
                                chart_id,
                                &level,
                                result.num_of_notes,
                                &result,
                                self.res.config.speed,
                                self.res.config.mods.bits(),
                                self.res.config.use_keyboard,
                                crate::huamei::encode_judgements(&self.judge),
                                crate::huamei::device_info(),
                                {
                                    let chart_ms = (self.res.track_length * 1000.).max(0.) as u32;
                                    let wall_ms = (self.play_wall * 1000.).max(0.) as u32;
                                    let ratio_x100 = if wall_ms > 0 {
                                        ((chart_ms as f64 / wall_ms as f64) * 100.).clamp(0., 65535.) as u16
                                    } else {
                                        0
                                    };
                                    crate::huamei::TimingInfo {
                                        wall_ms,
                                        chart_ms,
                                        ratio_x100,
                                        pause_count: self.pause_count,
                                        dt_outliers: self.dt_outliers,
                                    }
                                },
                            );
                            match payload.to_json_bytes() {
                                Ok(bytes) => record_data = Some(bytes),
                                Err(err) => warn!("huamei: 对局载荷序列化失败：{err}"),
                            }
                        }
                    }
                    // ══════════════════════════════════════════════════════

                    let record = if self.res.config.mods.intersects(Mods::UNRATED) || self.res.config.speed < 1.0 - 1e-3 {
                        None
                    } else {
                        Some(SimpleRecord {
                            score: result.score as _,
                            accuracy: result.accuracy as _,
                            full_combo: result.max_combo == result.num_of_notes,
                        })
                    };
                    self.next_scene = match self.mode {
                        GameMode::Normal | GameMode::NoRetry | GameMode::View => {
                            let historic_best = self.player.as_ref().map_or(0, |it| it.historic_best);
                            if let Some(new_rec) = &record {
                                if let Some(f) = &self.save_fn {
                                    f(new_rec.clone())?;
                                }
                                if let Some(best) = &mut self.best_record {
                                    best.update(new_rec);
                                } else {
                                    self.best_record = record.clone();
                                }
                                if let Some(best) = &self.best_record {
                                    if let Some(player) = &mut self.player {
                                        player.historic_best = player.historic_best.max(best.score as _);
                                    }
                                }
                            }
                            Some(NextScene::Overlay(Box::new(EndingScene::new(
                                self.res.background.clone(),
                                self.res.illustration.clone(),
                                self.res.player.clone(),
                                self.res.icons.clone(),
                                self.res.icon_retry.clone(),
                                self.res.icon_proceed.clone(),
                                self.res.mod_icons.clone(),
                                self.res.info.clone(),
                                self.judge.result(),
                                &self.res.config,
                                self.res.res_pack.ending.clone(),
                                self.upload_fn.as_ref().map(Arc::clone),
                                self.player.as_ref().map(|it| it.rks),
                                historic_best,
                                record_data,
                                self.best_record.clone(),
                                if self.res.config.show_avg_fps { self.get_avg_fps() } else { None },
                            )?)))
                        }
                        GameMode::TweakOffset => Some(NextScene::PopWithResult(Box::new(None::<f32>))),
                        GameMode::Exercise => None,
                    };
                }
                self.res.alpha = (1. - (t / AFTER_TIME).min(1.).powi(2)) as f32;
                self.res.track_length
            }
        };
        let time = (time - offset as f64).max(0.);
        self.res.time = time;
        if !tm.paused() && self.pause_rewind.is_none() && self.mode != GameMode::View {
            self.gl.quad_gl.viewport(self.res.camera.viewport);
            self.judge.update(&mut self.res, &mut self.chart, &mut self.bad_notes);
            self.gl.quad_gl.viewport(None);
            /* 【话梅】把这一帧的判定喂给实时判定条。
               事件队列和 judgements（上报链）完全独立，不影响成绩。 */
            if self.res.config.show_timing_bar {
                let real = tm.real_time();
                for ev in self.judge.take_timing_events() {
                    self.timing_bar.record(&ev, real, 3.0);
                }
                self.timing_bar.animate(real, 3.0);
            }
        }
        if let Some(update) = &mut self.update_fn {
            update(self.res.time, &mut self.res, &mut self.judge);
        }
        let counts = self.judge.counts();
        self.res.judge_line_color = if counts[2] + counts[3] == 0 && self.res.config.ap_fc_indicator {
            if counts[1] == 0 {
                self.res.res_pack.info.color_perfect()
            } else {
                self.res.res_pack.info.color_good()
            }
        } else {
            WHITE
        };
        if !self.dead
            && matches!(self.state, State::Playing)
            && (self.res.config.mods.contains(Mods::INSTANT_DEATH_AP) && counts[1] + counts[2] + counts[3] > 0
                || self.res.config.mods.contains(Mods::INSTANT_DEATH_FC) && counts[2] + counts[3] > 0)
        {
            if !self.music.paused() {
                self.music.pause()?;
            }
            tm.pause();
            self.dead = true;
            #[cfg(target_env = "ohos")]
            miniquad::native::set_interceptor_state(false);
            show_message(tl!("game-over")).error();
        }
        self.res.judge_line_color.a *= self.res.alpha;
        self.chart.update(&mut self.res);
        let res = &mut self.res;
        if res.config.interactive && is_key_pressed(KeyCode::Space) {
            if tm.paused() {
                if matches!(self.state, State::Playing) {
                    self.music.play()?;
                    tm.resume();
                }
            } else if matches!(self.state, State::Playing | State::BeforeMusic) {
                if !self.music.paused() {
                    self.music.pause()?;
                }
                tm.pause();
            }
        }
        if Self::interactive(res, &self.state) {
            if is_key_pressed(KeyCode::Left) && res.config.use_keyboard {
                res.time -= 1.;
                let dst = (self.music.position() - 1.).max(0.);
                self.music.seek_to(dst)?;
                tm.seek_to(dst);
            }
            if is_key_pressed(KeyCode::Right) && res.config.use_keyboard {
                res.time += 5.;
                let dst = (self.music.position() + 5.).min(res.track_length);
                self.music.seek_to(dst)?;
                tm.seek_to(dst);
            }
            if is_key_pressed(KeyCode::Q) {
                self.should_exit = true;
            }
        }
        for e in &mut self.effects {
            e.update(&self.res);
        }
        if let Some((id, text)) = take_input() {
            let offset = self.offset().min(0.);
            match id.as_str() {
                "exercise_start" => {
                    if let Some(t) = parse_time(&text) {
                        if !(offset as f64..self.res.track_length.min(self.exercise_range.end - 3.).max(offset as f64)).contains(&t) {
                            show_message(tl!("ex-time-out-of-range")).error();
                        } else {
                            self.exercise_range.start = t;
                            show_message(tl!("ex-time-set")).ok();
                        }
                    } else {
                        show_message(tl!("ex-invalid-format")).error();
                    }
                }
                "exercise_end" => {
                    if let Some(t) = parse_time(&text) {
                        if !((self.exercise_range.start + 3.).max(offset as f64).min(self.res.track_length)..self.res.track_length).contains(&t) {
                            show_message(tl!("ex-time-out-of-range")).error();
                        } else {
                            self.exercise_range.end = t;
                            show_message(tl!("ex-time-set")).ok();
                        }
                    } else {
                        show_message(tl!("ex-invalid-format")).error();
                    }
                }
                _ => return_input(id, text),
            }
        }
        Ok(())
    }

    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        if self.mode == GameMode::TweakOffset {
            self.offset_analysis.touch(touch, tm.real_time() as f32);
        }
        if self.mode == GameMode::Exercise && tm.paused() {
            let touch = Touch {
                position: touch.position * self.touch_scale(),
                ..touch.clone()
            };
            if self.exercise_btns.0.touch(&touch) {
                request_input("exercise_start", InputBox::new().default_text(fmt_time(self.exercise_range.start as f32)));
                return Ok(true);
            }
            if self.exercise_btns.1.touch(&touch) {
                request_input("exercise_end", InputBox::new().default_text(fmt_time(self.exercise_range.end as f32)));
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()> {
        if self.res.config.show_avg_fps {
            let current_time = tm.real_time();
            if matches!(self.state, State::Playing) && !tm.paused() {
                let frame_delta = current_time - self.fps_last_frame_time;
                self.fps_total_time += frame_delta;
                self.fps_frame_count += 1;
            }
            self.fps_last_frame_time = current_time;
        }

        /* 【话梅 TAA】抖动必须在借用 self.res 之前算 —— 之后 self 被长期借用。 */
        let taa_on = self.res.config.taa && self.res.config.sample_count <= 1;
        if taa_on {
            /* 历史纹理尺寸必须和**离屏目标**一致（不是 ui.viewport）：
               渲染精度 < 100% 时离屏目标被缩小，两者不一致会让历史帧被拉伸，
               在画面上表现为一块块错位的重影。 */
            let dim = self
                .res
                .chart_target
                .as_ref()
                .map(|it| it.dim())
                .unwrap_or((ui.viewport.2 as u32, ui.viewport.3 as u32));
            self.taa.ensure_history(dim);
        }
        let taa_jitter = if taa_on {
            self.taa.jitter((ui.viewport.2 as u32, ui.viewport.3 as u32))
        } else {
            Vec2::ZERO
        };
        let taa_weight = self.taa.history_weight(self.res.config.speed * 2.);
        let taa_history = self.taa.history();

        let res = &mut self.res;
        let asp = ui.viewport.2 as f32 / ui.viewport.3 as f32;
        /* blit 段要用它，但那时不能再碰 res（否则借用一直拖到 self.ui()，冲突）。
           提前取出来。 */
        let render_scale_cfg = res.config.render_scale;
        if res.update_size(crate::huamei::safe_viewport(ui.viewport)) || self.mode == GameMode::View {
            set_camera(&res.camera);
        }

        let msaa = res.config.sample_count > 1;
        let taa = taa_on; // TAA 与 MSAA 互斥（见上）

        let chart_onto = res
            .chart_target
            .as_ref()
            .map(|it| if msaa { it.input() } else { it.output() })
            .or(res.camera.render_target);
        push_camera_state();
        /* ① 【话梅 TAA】亚像素抖动：偏移谱面相机（不含 HUD）。
           没有这一步，下面的混合只会得到运动模糊，不是抗锯齿。 */
        let jitter = taa_jitter;
        set_camera(&Camera2D {
            zoom: vec2(1., -asp),
            target: jitter,
            viewport: if res.chart_target.is_some() { None } else { Some(ui.viewport) },
            render_target: chart_onto,
            ..Default::default()
        });
        /* 不清屏 —— 紧接着的 draw_background 是一张 CropCenter 铺满全屏的图，
           会 100% 覆盖上一帧。原来这里每帧多一次全屏写入（3x 屏 ≈ 560 万像素），
           在 iPad 上是纯浪费：填充率正是那里最紧的资源。
           ⚠ 只有当背景确实铺满时才成立；BLACK_TEXTURE 兜底也是铺满的。 */
        draw_background(*res.background);
        pop_camera_state();

        let chart_target_vp = if res.chart_target.is_some() {
            let vp = res.camera.viewport.unwrap();
            /* 【话梅】渲染精度：离屏目标被缩小了，viewport 必须同步缩小，
               否则只会画到左上角一块（剩下的黑边）。 */
            let sc = if res.config.render_scale.is_finite() { res.config.render_scale.clamp(0.6, 1.) } else { 1. };
            Some((
                ((vp.0 - ui.viewport.0) as f32 * sc).round() as i32,
                ((vp.1 - ui.viewport.1) as f32 * sc).round() as i32,
                ((vp.2 as f32 * sc).round() as i32).max(1),
                ((vp.3 as f32 * sc).round() as i32).max(1),
            ))
        } else {
            res.camera.viewport
        };
        self.gl.quad_gl.render_pass(chart_onto.map(|it| it.render_pass));
        self.gl.quad_gl.viewport(chart_target_vp);

        let h = 1. / res.aspect_ratio;
        draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., res.alpha * res.info.background_dim));

        self.chart.render(ui, res);

        /* 【话梅】判定范围可视化：紧跟在谱面绘制之后、同一个相机里画，
           这样坐标与音符完全对齐。纯只读叠加，不碰 note/judge 状态。 */
        if res.config.show_judgement_range {
            crate::judgement_range::render(&self.chart, res);
        }

        self.gl.quad_gl.render_pass(
            res.chart_target
                .as_ref()
                .map(|it| it.output().render_pass)
                .or_else(|| res.camera.render_pass()),
        );

        self.bad_notes.retain(|dummy| dummy.render(res));
        let t = tm.real_time();
        let dt = (t - std::mem::replace(&mut self.last_update_time, t)) as f32;
        /* 计时采样：只统计「游玩中且未暂停」的真实时间。
           暂停不算 —— 否则挂机十分钟会被误判成「拖慢」。 */
        {
            let paused = tm.paused();
            if paused && !self.was_paused {
                self.pause_count = self.pause_count.saturating_add(1);
            }
            self.was_paused = paused;
            if matches!(self.state, State::Playing) && !paused {
                self.play_wall += dt as f64;
                if dt > 0.1 {
                    self.dt_outliers = self.dt_outliers.saturating_add(1);
                }
            }
        }
        if res.config.particle && !res.config.low_quality {
            res.emitter.draw(dt);
        }
        self.ui(ui, tm)?;
        self.overlay_ui(ui, tm)?;
        /* 【话梅】实时判定条：切到屏幕坐标系画，避开判定线的谱面相机。 */
        if self.res.config.show_timing_bar && !tm.paused() {
            push_camera_state();
            set_camera(&ui.camera());
            let (early, late) = self.judge.early_late_counts();
            ui.abs_scope(|ui| {
                crate::timing_bar::render(ui, &self.timing_bar, 1.0, early, late);
            });
            pop_camera_state();
        }

        if self.mode == GameMode::TweakOffset {
            push_camera_state();
            self.gl.quad_gl.viewport(None);
            set_camera(&Camera2D {
                zoom: vec2(1., -screen_aspect()),
                render_target: self.res.chart_target.as_ref().map(|it| it.output()).or(self.res.camera.render_target),
                ..Default::default()
            });
            self.tweak_offset(ui, Self::interactive(&self.res, &self.state));
            pop_camera_state();
        }

        if !self.res.no_effect && !self.effects.is_empty() {
            push_camera_state();
            set_camera(&Camera2D {
                zoom: vec2(1., asp),
                ..Default::default()
            });
            for e in &self.effects {
                e.render(&mut self.res);
            }
            pop_camera_state();
        }
        if msaa || taa || !self.res.no_effect {
            // render the texture onto screen
            if let Some(target) = &mut self.res.chart_target {
                self.gl.flush();
                push_camera_state();
                self.gl.quad_gl.viewport(None);
                set_camera(&Camera2D {
                    zoom: vec2(1., asp),
                    render_target: self.res.camera.render_target,
                    viewport: Some(ui.viewport),
                    ..Default::default()
                });
                /* 【话梅】放大这一遍：渲染精度 < 100% 时用 bicubic 上采样，
                   而不是默认的双线性。双线性在放大后边缘发软、判定线会糊；
                   bicubic 能明显找回锐度 —— 这是 MetalFX Spatial 在
                   GLES 管线里的等效做法（详见 upscale.rs 顶部说明）。 */
                let scale = if render_scale_cfg.is_finite() { render_scale_cfg.clamp(0.6, 1.) } else { 1. };
                let use_up = scale < 0.999;
                if use_up {
                    let (iw, ih) = target.dim();
                    /* ⚠ 创建失败必须记住失败，否则每帧都会重试编译 shader ——
                       那是一次完整的 GLSL 编译 + 链接，足够把帧率拖到个位数。
                       某些 GLES 实现（老设备、驱动限制）确实会失败。 */
                    if self.res.upscaler.is_none() && !self.res.upscaler_failed {
                        match crate::upscale::Upscaler::new((iw as f32, ih as f32)) {
                            Ok(it) => self.res.upscaler = Some(it),
                            Err(err) => {
                                tracing::warn!(?err, "上采样 shader 创建失败，回退双线性");
                                self.res.upscaler_failed = true;
                            }
                        }
                    }
                    if let Some(up) = &mut self.res.upscaler {
                        up.set_input_size((iw as f32, ih as f32));
                        up.draw(target.output().texture, ui.top);
                    }
                } else {
                    draw_texture_ex(
                        target.output().texture,
                        -1.,
                        -ui.top,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(2., ui.top * 2.)),
                            ..Default::default()
                        },
                    );
                }
                /* ④ 【话梅 TAA】时间累积混合。
                   先画历史（权重 1-α），再把当前帧以 α 叠上去。
                   顺序很重要：反过来的话历史会盖住当前帧的细节。
                   α 由滚动速度决定（③ 的近似），静止时更依赖历史。 */
                if taa {
                    let w = taa_weight;
                    if let Some(hist) = taa_history {
                        draw_texture_ex(
                            hist,
                            -1.,
                            -ui.top,
                            Color { a: 1. - w, ..WHITE },
                            DrawTextureParams {
                                dest_size: Some(vec2(2., ui.top * 2.)),
                                ..Default::default()
                            },
                        );
                    }
                }
                pop_camera_state();

                /* 收尾：把当前帧写进历史纹理。少了这一步，历史永远是空的，
                   下一帧的混合就什么都没叠上去（看起来像没生效）。 */
                if taa {
                    let hist = self.taa.history_target();
                    let cur = target.output().texture;
                    if let Some(hist) = hist {
                        push_camera_state();
                        /* ⚠ zoom.y 必须是**正数**。
                           踩过：写成 vec2(1., -1.) 会把历史帧上下翻转一次，
                           读到屏幕上就是「整个画面镜像 + 重影」（音符和数字都倒着）。
                           这里要和下面「贴到屏幕」用同一套朝向，
                           否则 TAA 混合出来的就是一张倒过来的旧画面。 */
                        set_camera(&Camera2D {
                            zoom: vec2(1., 1.),
                            render_target: Some(hist),
                            ..Default::default()
                        });
                        clear_background(BLACK);
                        draw_texture_ex(
                            cur,
                            -1.,
                            -1.,
                            WHITE,
                            DrawTextureParams {
                                dest_size: Some(vec2(2., 2.)),
                                ..Default::default()
                            },
                        );
                        pop_camera_state();
                    }
                }
                self.taa.advance();
            }
        }
        Ok(())
    }

    /* 【话梅 2026-11-17 诊断「退出后仍有声音」】
       假设：sasa 的 Sfx / Music 内部都是 Arc 共享给音频线程，
       **只有 drop AudioManager（关闭音频流）才能让所有声音立即停止**。
       原版能停 = GameScene 的 Resource 被正常释放；
       我们这版不停 = 某个东西还在持有 GameScene。

       这条日志用来确认「退出后 GameScene 到底有没有被 drop」：
       · 打出 → 释放正常，声音残留另有原因（长音效自然播完）
       · 打不出 → 有引用泄漏，需要查 Task / record_data 等持有者 */
    fn next_scene(&mut self, tm: &mut TimeManager) -> NextScene {
        /* 【话梅 2026-11-17】离开对局**必须显式暂停音乐**。
           sasa 的 Music 内部是 `Arc` 共享给音频渲染线程 ——
           **drop 不会停止播放**，所以只要不 pause，回大厅/退出展示谱面后
           曲子会一直放下去（用户实测：返回大厅后音频还在响）。
           覆盖第三条路径：完成结算退出、暂停菜单返回、展示模式(View)退出。 */
        let leaving = self.should_exit
            || matches!(&self.next_scene, Some(n) if !matches!(n, NextScene::None));
        if leaving {
            /* 双保险：pause 让播放头停住；set_amplifier(0) 立刻静音。
               为什么两个都要：
               · pause 走的是 sasa 的命令队列，命令到达音频线程前
                 仍有缓冲在响；拉到 0 是立即的
               · 反过来只有静音不 pause，曲子会在后台继续走位置，
                 下次进来就对不上（用户实测：等到曲子放完再进就不同步）

               恢复在 GameScene 重新进入时做（见 enter/update 里的
               set_amplifier(volume_music)），所以这里置 0 是安全的。 */
            if let Err(err) = self.music.pause() {
                tracing::warn!("next_scene: 暂停音乐失败 {err}");
            }
            if let Err(err) = self.music.set_amplifier(0.) {
                tracing::warn!("next_scene: 静音失败 {err}");
            }
            tracing::info!("game: 离开对局，已暂停并静音音乐");
        }

        if self.should_exit {
            if tm.paused() {
                tm.resume();
            }
            tm.speed = 1.0;
            tm.adjust_time = false;
            match self.mode {
                // return result to update score and refresh
                GameMode::Normal => {
                    if let Some(rec) = &self.best_record {
                        NextScene::PopWithResult(Box::new(rec.clone()))
                    } else {
                        NextScene::Pop
                    }
                }
                // not sure if they need result. just keep it
                GameMode::Exercise | GameMode::NoRetry | GameMode::View => NextScene::Pop,
                GameMode::TweakOffset => NextScene::PopWithResult(Box::new(None::<f32>)),
            }
        } else if let Some(next_scene) = self.next_scene.take() {
            if !matches!(next_scene, NextScene::None) && tm.paused() {
                tm.resume();
            }
            tm.speed = 1.0;
            tm.adjust_time = false;
            next_scene
        } else {
            NextScene::None
        }
    }
}

impl Drop for GameScene {
    fn drop(&mut self) {
        tracing::info!(
            "game: GameScene 已释放（music 是否已暂停={}）—— 若此后仍有声音，说明音频流未关闭",
            self.music.paused()
        );
    }
}
