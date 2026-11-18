use super::{BpmList, Effect, JudgeLine, JudgeLineKind, Matrix, Resource, UIElement, Vector};
use crate::{core::Object, fs::FileSystem, judge::JudgeStatus, ui::Ui};
use anyhow::{Context, Result};
use macroquad::prelude::*;
use nalgebra::Rotation2;
use sasa::AudioClip;
use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
pub struct ChartExtra {
    pub effects: Vec<Effect>,
    pub global_effects: Vec<Effect>,
    #[cfg(feature = "video")]
    pub videos: Vec<(super::Video, Option<super::VideoAttach>)>,
}

#[derive(Default)]
pub struct ChartSettings {
    pub pe_alpha_extension: bool,
    pub hold_partial_cover: bool,
}

pub type HitSoundMap = HashMap<String, AudioClip>;

pub struct Chart {
    pub offset: f32,
    pub lines: Vec<JudgeLine>,
    pub bpm_list: RefCell<BpmList>,

    pub settings: ChartSettings,
    pub extra: ChartExtra,

    /// Line order according to z-index, lines with attach_ui will be removed from this list
    ///
    /// Store the index of the line in z-index ascending order
    pub order: Vec<usize>,
    /// TODO: docs from RPE
    pub attach_ui: [Option<usize>; 7],

    pub hitsounds: HitSoundMap,
}

/// 【话梅 2026-11-18】从 PhiraiAd 移植：按键转换的**原始类型备份**。
///
/// 为什么需要备份：`NoteConversion` 会就地改写每个 note 的 `kind`，
/// 而玩家可能在设置里反复切换（原版 → 全 Tap → 全 Drag → 原版）。
/// 没有原始快照的话，切回"原版"时已经拿不回原来的类型了 ——
/// 那就只能重新解析谱面文件（很慢，且会丢失运行时状态）。
///
/// 快照只存 (kind, hitsound)，按判定线分组，与 notes 一一对应 ——
/// 所以 `JudgeLineCache::with_companion` 排序时必须**同步重排**它。
pub struct NoteConversionBackup(Vec<Vec<(super::NoteKind, crate::judge::HitSound)>>);

impl NoteConversionBackup {
    /// 在第一次应用转换**之前**调用，把谱面原始状态存下来。
    pub fn capture(chart: &Chart) -> Self {
        Self(
            chart
                .lines
                .iter()
                .map(|line| {
                    line.notes
                        .iter()
                        .map(|n| (n.kind.clone(), n.hitsound.clone()))
                        .collect()
                })
                .collect(),
        )
    }

    /// 按模式改写所有音符类型；`Original` 时原样还原。
    pub fn apply(&mut self, chart: &mut Chart, mode: crate::chart_play::NoteConversion) {
        use crate::chart_play::NoteConversion;
        for (line, originals) in chart.lines.iter_mut().zip(&mut self.0) {
            for (note, (kind, sound)) in line.notes.iter_mut().zip(originals.iter()) {
                note.kind = match mode {
                    NoteConversion::Original => kind.clone(),
                    NoteConversion::Tap => super::NoteKind::Click,
                    NoteConversion::Drag => super::NoteKind::Drag,
                    NoteConversion::Flick => super::NoteKind::Flick,
                };
                /* 转换后原 hitsound 可能不再匹配新类型（比如 Tap 用了
                   Flick 的音效），所以非原版统一回落到该类型的默认音效。 */
                note.hitsound = if mode == NoteConversion::Original {
                    sound.clone()
                } else {
                    crate::judge::HitSound::default_from_kind(&note.kind)
                };
            }
            line.cache = super::JudgeLineCache::with_companion(&mut line.notes, originals);
        }
    }
}

impl Chart {
    pub fn new(offset: f32, lines: Vec<JudgeLine>, bpm_list: BpmList, settings: ChartSettings, extra: ChartExtra, hitsounds: HitSoundMap) -> Self {
        let mut attach_ui = [None; 7];
        let mut order = (0..lines.len())
            .filter(|it| {
                if let Some(element) = lines[*it].attach_ui {
                    attach_ui[element as usize - 1] = Some(*it);
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();
        order.sort_by_key(|it| (lines[*it].z_index, *it));
        Self {
            offset,
            lines,
            bpm_list: RefCell::new(bpm_list),
            settings,
            extra,

            order,
            attach_ui,

            hitsounds,
        }
    }

    #[inline]
    pub fn with_element<R>(
        &self,
        ui: &mut Ui,
        res: &Resource,
        element: UIElement,
        scale_point: Option<(f32, f32)>,
        rotation_point: (f32, f32),
        f: impl FnOnce(&mut Ui, Color) -> R,
    ) -> R {
        let scale_point = scale_point.unwrap_or(rotation_point);
        if let Some(id) = self.attach_ui[element as usize - 1] {
            let lines = &self.lines;
            let line = &lines[id];
            let obj = &line.object;
            let mut tr = line.fetch_pos(res, lines);
            tr.y = -tr.y;
            let color = self.lines[id].color.now_opt().unwrap_or(WHITE);
            let scale = obj.now_scale(Vector::new(scale_point.0, scale_point.1));
            let ro =
                Object::new_rotation_wrt_point(Rotation2::new(-obj.rotation.now().to_radians()), Vector::new(rotation_point.0, rotation_point.1));
            ui.with(Matrix::new_translation(&tr) * ro * scale, |ui| ui.alpha(obj.now_alpha().max(0.), |ui| f(ui, color)))
        } else {
            f(ui, WHITE)
        }
    }

    pub async fn load_textures(&mut self, fs: &mut dyn FileSystem) -> Result<()> {
        for line in &mut self.lines {
            if let JudgeLineKind::Texture(tex, path) = &mut line.kind {
                *tex = image::load_from_memory(&fs.load_file(path).await.with_context(|| format!("failed to load illustration {path}"))?)?.into();
            }
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        self.lines
            .iter_mut()
            .flat_map(|it| it.notes.iter_mut())
            .for_each(|note| note.judge = JudgeStatus::NotJudged);
        for line in &mut self.lines {
            line.cache.reset(&mut line.notes);
        }
        #[cfg(feature = "video")]
        for (video, _) in &mut self.extra.videos {
            if let Err(err) = video.reset() {
                use crate::parse::{ptl, L10N_LOCAL};
                crate::scene::show_error(err.context(ptl!("video-load-failed", "path" => video.video_file.path().to_string_lossy())));
            }
        }
    }

    pub fn update(&mut self, res: &mut Resource) {
        for line in &mut self.lines {
            line.object.set_time(res.time);
        }
        // TODO optimize
        let trs = self.lines.iter().map(|it| it.now_transform(res, &self.lines)).collect::<Vec<_>>();
        let rotations = self.lines.iter().map(|it| it.fetch_rot(&self.lines)).collect::<Vec<_>>();
        for ((line, tr), rot) in self.lines.iter_mut().zip(trs).zip(rotations) {
            line.update(res, tr, rot);
        }
        for effect in &mut self.extra.effects {
            effect.update(res);
        }
        #[cfg(feature = "video")]
        for (video, _) in &mut self.extra.videos {
            if let Err(err) = video.update(res.time) {
                tracing::warn!("video error: {err:?}");
            }
        }
    }

    pub fn render(&self, ui: &mut Ui, res: &mut Resource) {
        #[cfg(feature = "video")]
        for (video, attach) in &self.extra.videos {
            if let Some(attach) = attach {
                let line = &self.lines[attach.line];
                let color = line.color.now_opt().unwrap_or(res.judge_line_color);
                let mat = self.lines[attach.line].object.now(res);
                res.apply_model_of(&mat, |res| {
                    video.render(res.time, res.aspect_ratio, color);
                });
            } else {
                video.render(res.time, res.aspect_ratio, WHITE);
            }
        }
        res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(if res.config.flip_x() { -1. } else { 1. }, -1.)), |res| {
            let mut guard = self.bpm_list.borrow_mut();
            for id in &self.order {
                self.lines[*id].render(ui, res, &self.lines, &mut guard, &self.settings, *id);
            }
            drop(guard);
            res.note_buffer.borrow_mut().draw_all();
            if res.config.sample_count > 1 {
                unsafe { get_internal_gl() }.flush();
                if let Some(target) = &res.chart_target {
                    target.blit();
                }
            }
            if !res.no_effect {
                let render = |res: &mut Resource| {
                    for effect in &self.extra.effects {
                        effect.render(res);
                    }
                };
                if res.config.flip_x() {
                    res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(-1., 1.)), render);
                } else {
                    render(res);
                }
            }
        });
    }
}
