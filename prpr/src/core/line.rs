use super::{chart::ChartSettings, object::CtrlObject, Anim, AnimFloat, BpmList, Matrix, Note, Object, Point, RenderConfig, Resource, Vector};
use crate::{
    ext::{get_viewport, NotNanExt, SafeTexture},
    judge::JudgeStatus,
    ui::Ui,
};
use macroquad::prelude::*;
use miniquad::{RenderPass, Texture, TextureParams, TextureWrap};
use nalgebra::Rotation2;
use serde::Deserialize;
use std::cell::RefCell;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum UIElement {
    Pause = 1,
    ComboNumber = 2,
    Combo = 3,
    Score = 4,
    Bar = 5,
    Name = 6,
    Level = 7,
}

impl UIElement {
    pub fn from_u8(val: u8) -> Option<Self> {
        Some(match val {
            1 => Self::Pause,
            2 => Self::ComboNumber,
            3 => Self::Combo,
            4 => Self::Score,
            5 => Self::Bar,
            6 => Self::Name,
            7 => Self::Level,
            _ => return None,
        })
    }
}

pub struct GifFrames {
    /// time of each frame in milliseconds
    frames: Vec<(u128, SafeTexture)>,
    /// milliseconds
    total_time: u128,
}

impl GifFrames {
    pub fn new(frames: Vec<(u128, SafeTexture)>) -> Self {
        let total_time = frames.iter().map(|(time, _)| *time).sum();
        Self { frames, total_time }
    }

    pub fn get_time_frame(&self, time: u128) -> &SafeTexture {
        let mut time = time % self.total_time;
        for (t, frame) in &self.frames {
            if time < *t {
                return frame;
            }
            time -= t;
        }
        &self.frames.last().unwrap().1
    }

    pub fn get_prog_frame(&self, prog: f32) -> &SafeTexture {
        let time = (prog * self.total_time as f32) as u128;
        self.get_time_frame(time)
    }

    pub fn total_time(&self) -> u128 {
        self.total_time
    }
}

#[derive(Default)]
pub enum JudgeLineKind {
    #[default]
    Normal,
    Texture(SafeTexture, String),
    TextureGif(Anim<f32>, GifFrames, String),
    Text(Anim<String>),
    Paint(Anim<f32>, RefCell<(Option<RenderPass>, bool)>),
}

#[derive(Clone)]
pub struct JudgeLineCache {
    update_order: Vec<u32>,
    not_plain_count: usize,
    above_indices: Vec<usize>,
    below_indices: Vec<usize>,
}

impl JudgeLineCache {
    pub fn new(notes: &mut [Note]) -> Self {
        notes
            .sort_by_key(|it| (it.plain(), !it.above, it.speed.not_nan(), ((it.height + it.object.translation.1.now() as f64) * it.speed).not_nan()));
        let mut res = Self {
            update_order: Vec::new(),
            not_plain_count: 0,
            above_indices: Vec::new(),
            below_indices: Vec::new(),
        };
        res.reset(notes);
        res
    }

    pub(crate) fn reset(&mut self, notes: &mut [Note]) {
        self.update_order = (0..notes.len() as u32).collect();
        self.above_indices.clear();
        self.below_indices.clear();
        let mut index = notes.iter().position(|it| it.plain()).unwrap_or(notes.len());
        self.not_plain_count = index;
        while notes.get(index).is_some_and(|it| it.above) {
            self.above_indices.push(index);
            let speed = notes[index].speed;
            loop {
                index += 1;
                if !notes.get(index).is_some_and(|it| it.above && it.speed == speed) {
                    break;
                }
            }
        }
        while index != notes.len() {
            self.below_indices.push(index);
            let speed = notes[index].speed;
            loop {
                index += 1;
                if !notes.get(index).is_some_and(|it| it.speed == speed) {
                    break;
                }
            }
        }
    }
}

pub struct JudgeLine {
    pub object: Object,
    pub ctrl_obj: RefCell<CtrlObject>,
    pub kind: JudgeLineKind,
    /// Height Animation, decribes the `height` of the line at a specific time
    ///
    /// The `height` here can be considered as the absolute 'y' coordinate of the notes attached to this line, which is calculated by
    /// ∫ v(t) dt, where v(t) is the speed of the line at time t.
    pub height: AnimFloat,
    pub incline: AnimFloat,
    pub notes: Vec<Note>,
    pub color: Anim<Color>,
    pub parent: Option<usize>,
    pub rot_with_parent: bool,
    pub z_index: i32,
    /// Whether to show notes below the line, here below is defined in the time axis, which means the note should already be judged
    ///
    /// TODO: Not sure
    pub show_below: bool,
    pub attach_ui: Option<UIElement>,

    pub cache: JudgeLineCache,
}

impl JudgeLine {
    pub fn update(&mut self, res: &mut Resource, tr: Matrix, parent_rot: f32) {
        // self.object.set_time(res.time); // this is done by chart, chart has to calculate transform for us
        self.height.set_time(res.time);
        let line_height = self.height.now();
        let mut ctrl_obj = self.ctrl_obj.borrow_mut();
        self.cache.update_order.retain(|id| {
            let note = &mut self.notes[*id as usize];
            note.update(res, parent_rot, &tr, &mut ctrl_obj, line_height as f64);
            !note.dead()
        });
        drop(ctrl_obj);
        match &mut self.kind {
            JudgeLineKind::Text(anim) => {
                anim.set_time(res.time);
            }
            JudgeLineKind::Paint(anim, ..) => {
                anim.set_time(res.time);
            }
            JudgeLineKind::TextureGif(anim, ..) => {
                anim.set_time(res.time);
            }
            _ => {}
        }
        self.color.set_time(res.time);
        self.cache.above_indices.retain_mut(|index| {
            while matches!(self.notes[*index].judge, JudgeStatus::Judged) {
                if self
                    .notes
                    .get(*index + 1)
                    .is_some_and(|it| it.above && it.speed == self.notes[*index].speed)
                {
                    *index += 1;
                } else {
                    return false;
                }
            }
            true
        });
        self.cache.below_indices.retain_mut(|index| {
            while matches!(self.notes[*index].judge, JudgeStatus::Judged) {
                if self.notes.get(*index + 1).is_some_and(|it| it.speed == self.notes[*index].speed) {
                    *index += 1;
                } else {
                    return false;
                }
            }
            true
        });
    }

    pub fn fetch_rot(&self, lines: &[JudgeLine]) -> f32 {
        let mut rot = self.object.rotation.now();
        if self.rot_with_parent {
            if let Some(parent) = self.parent {
                rot += lines[parent].fetch_rot(lines);
            }
        }
        rot
    }

    pub fn fetch_pos(&self, res: &Resource, lines: &[JudgeLine]) -> Vector {
        if let Some(parent) = self.parent {
            let parent = &lines[parent];
            let parent_translation = parent.fetch_pos(res, lines);
            return parent_translation + Rotation2::new(parent.fetch_rot(lines).to_radians()) * self.object.now_translation(res);
        }
        self.object.now_translation(res)
    }

    pub fn now_transform(&self, res: &Resource, lines: &[JudgeLine]) -> Matrix {
        Rotation2::new(self.fetch_rot(lines).to_radians())
            .to_homogeneous()
            .append_translation(&self.fetch_pos(res, lines))
    }

    pub fn render(&self, ui: &mut Ui, res: &mut Resource, lines: &[JudgeLine], bpm_list: &mut BpmList, settings: &ChartSettings, id: usize) {
        let alpha = self.object.alpha.now_opt().unwrap_or(1.0) * res.alpha;
        let color = self.color.now_opt();
        let line_scaled = (self.object.scale.1.now() - 1.).abs() > 1e-4;
        res.with_model(self.now_transform(res, lines), |res| {
            if res.config.chart_debug {
                res.apply_model(|_| {
                    ui.text(id.to_string()).pos(0., -0.01).anchor(0.5, 1.).size(0.8).draw();
                });
            }
            res.with_model(self.object.now_scale(Vector::default()), |res| {
                res.apply_model(|res| match &self.kind {
                    JudgeLineKind::Normal => {
                        let mut color = color.unwrap_or(res.judge_line_color);
                        color.a *= alpha.max(0.0);
                        let len = res.info.line_length;
                        draw_line(-len, 0., len, 0., if line_scaled { 0.0076 } else { 0.01 }, color);
                    }
                    JudgeLineKind::Texture(texture, _) => {
                        let mut color = color.unwrap_or(WHITE);
                        color.a = alpha.max(0.0);
                        if color.a == 0.0 {
                            return;
                        }
                        let hf = vec2(texture.width(), texture.height());
                        draw_texture_ex(
                            **texture,
                            -hf.x / 2.,
                            -hf.y / 2.,
                            color,
                            DrawTextureParams {
                                dest_size: Some(hf),
                                flip_y: true,
                                ..Default::default()
                            },
                        );
                    }
                    JudgeLineKind::TextureGif(anim, frames, _) => {
                        let t = anim.now_opt().unwrap_or(0.0);
                        let frame = frames.get_prog_frame(t);
                        let mut color = color.unwrap_or(WHITE);
                        color.a = alpha.max(0.0);
                        let hf = vec2(frame.width(), frame.height());
                        draw_texture_ex(
                            **frame,
                            -hf.x / 2.,
                            -hf.y / 2.,
                            color,
                            DrawTextureParams {
                                dest_size: Some(hf),
                                flip_y: true,
                                ..Default::default()
                            },
                        );
                    }
                    JudgeLineKind::Text(anim) => {
                        let mut color = color.unwrap_or(WHITE);
                        color.a = alpha.max(0.0);
                        let now = anim.now();
                        res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(1., -1.)), |_| {
                            ui.text(&now).pos(0., 0.).anchor(0.5, 0.5).size(1.).color(color).multiline().draw();
                        });
                    }
                    JudgeLineKind::Paint(anim, state) => {
                        let mut color = color.unwrap_or(WHITE);
                        color.a = alpha.max(0.0) * 2.55;
                        let mut gl = unsafe { get_internal_gl() };
                        let mut guard = state.borrow_mut();
                        let vp = get_viewport();
                        let pass = *guard.0.get_or_insert_with(|| {
                            let ctx = &mut gl.quad_context;
                            let tex = Texture::new_render_texture(
                                ctx,
                                TextureParams {
                                    width: vp.2 as _,
                                    height: vp.3 as _,
                                    format: miniquad::TextureFormat::RGBA8,
                                    filter: FilterMode::Linear,
                                    wrap: TextureWrap::Clamp,
                                },
                            );
                            RenderPass::new(ctx, tex, None)
                        });
                        gl.flush();
                        let old_pass = gl.quad_gl.get_active_render_pass();
                        gl.quad_gl.render_pass(Some(pass));
                        gl.quad_gl.viewport(None);
                        let size = anim.now();
                        if size <= 0. {
                            if guard.1 {
                                clear_background(Color::default());
                                guard.1 = false;
                            }
                        } else {
                            ui.fill_circle(0., 0., size / vp.2 as f32 * 2., color);
                            guard.1 = true;
                        }
                        gl.flush();
                        gl.quad_gl.render_pass(old_pass);
                        gl.quad_gl.viewport(Some(vp));
                    }
                })
            });
            if let JudgeLineKind::Paint(_, state) = &self.kind {
                let guard = state.borrow_mut();
                if guard.1 {
                    let ctx = unsafe { get_internal_gl() }.quad_context;
                    let tex = guard.0.as_ref().unwrap().texture(ctx);
                    let top = 1. / res.aspect_ratio;
                    draw_texture_ex(
                        Texture2D::from_miniquad_texture(tex),
                        -1.,
                        -top,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(2., top * 2.)),
                            ..Default::default()
                        },
                    );
                }
            }
            let mut config = RenderConfig {
                settings,
                ctrl_obj: &mut self.ctrl_obj.borrow_mut(),
                line_height: self.height.now() as f64,
                appear_before: f64::INFINITY,
                draw_below: self.show_below,
                incline_sin: self.incline.now_opt().map(|it| it.to_radians().sin()).unwrap_or_default(),
            };
            if alpha < 0.0 {
                if !settings.pe_alpha_extension {
                    return;
                }
                let w = (-alpha).floor() as u32;
                match w {
                    1 => {
                        return;
                    }
                    2 => {
                        config.draw_below = false;
                    }
                    w if (100..1000).contains(&w) => {
                        config.appear_before = (w as f64 - 100.) / 10.;
                    }
                    w if (1000..2000).contains(&w) => {
                        // TODO unsupported
                    }
                    _ => {}
                }
            }
            /* ── 判定线上下各要渲染多少个谱面单位 ──
             *
             * 【上游怎么写的】
             *   upstream: height_above = screen_to_world(±1).y_max * aspect_ratio
             *   它拿 NDC 的 1.0 乘上 aspect_ratio —— 也就是说**故意多渲染 ~43%**
             *   （1.43 对可见的 1.0）。这份余量就是它的安全垫：判定线有位移、
             *   音符贴图有半高、甚至四舍五入误差，都不会露白。
             *
             * 【我们踩的坑】
             *   2026-11-16 我为了「精确」把 NDC 换算成世界坐标，得到 1.05 ——
             *   余量从 43% 掉到 5%。横屏本来靠这 43% 撑着，一缩就露馅；
             *   竖屏再叠加判定线偏移，直接空一大片。
             *   **不要动坐标系，只要保证「渲染范围 ≥ 可见范围 × 余量」。**
             *
             * 【现在的写法】
             *   直接由两个比例算，不经过 screen_to_world（那函数的单位太隐晦）：
             *     可见（世界）= 1/aspect_ratio，谱面 = × chart_aspect
             *     判定线自身位移 line_y 也是世界坐标，一并加进去
             *     最后 ×1.5 余量（上游是 1.43，取整到 1.5 更保险）
             *   横屏：结果与上游基本一致（1.5 vs 1.43）；竖屏：自动跟着变高。 */
            let line_y = self.fetch_pos(res, lines).y;
            let half_world = 1. / res.aspect_ratio;
            const RENDER_MARGIN: f32 = 1.5;
            /* ⚠ 符号：屏幕上边界在世界坐标是 +half_world，判定线在 line_y。
               判定线**以上**的可视范围 = half_world - line_y（线越靠下，上方越大）。
               2026-11-16 写反过一次：线一往下移，上限反而变小，横屏竖屏一起露白。 */
            let height_above =
                (half_world - line_y) * res.chart_aspect * RENDER_MARGIN;
            let height_below =
                (half_world + line_y) * res.chart_aspect * RENDER_MARGIN;
            let agg = res.config.aggressive;
            for note in self.notes.iter().take(self.cache.not_plain_count).filter(|it| it.above) {
                note.render(res, &mut config, bpm_list);
            }
            for index in &self.cache.above_indices {
                let speed = self.notes[*index].speed;
                let limit = height_above as f64 / speed;
                for note in self.notes[*index..].iter() {
                    if !note.above || speed != note.speed {
                        break;
                    }
                    if agg && note.height - config.line_height + note.object.translation.1.now() as f64 > limit {
                        break;
                    }
                    note.render(res, &mut config, bpm_list);
                }
            }
            res.with_model(Matrix::identity().append_nonuniform_scaling(&Vector::new(1.0, -1.0)), |res| {
                for note in self.notes.iter().take(self.cache.not_plain_count).filter(|it| !it.above) {
                    note.render(res, &mut config, bpm_list);
                }
                for index in &self.cache.below_indices {
                    let speed = self.notes[*index].speed;
                    let limit = height_below as f64 / speed;
                    for note in self.notes[*index..].iter() {
                        if speed != note.speed {
                            break;
                        }
                        if agg && note.height - config.line_height + note.object.translation.1.now() as f64 > limit {
                            break;
                        }
                        note.render(res, &mut config, bpm_list);
                    }
                }
            });
        });
    }
}
