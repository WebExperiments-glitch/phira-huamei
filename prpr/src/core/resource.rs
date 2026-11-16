use super::{MSRenderTarget, Matrix, Point, NOTE_WIDTH_RATIO_BASE};
use crate::{
    config::Config,
    ext::{create_audio_manger, nalgebra_to_glm, SafeTexture},
    fs::FileSystem,
    info::ChartInfo,
    particle::{AtlasConfig, ColorCurve, Emitter, EmitterConfig},
};
use anyhow::{bail, Context, Result};
use macroquad::prelude::*;
use miniquad::{
    gl::{GLuint, GL_LINEAR},
    Texture, TextureWrap,
};
use sasa::{AudioClip, AudioManager, Sfx};
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    ops::DerefMut,
    path::Path,
    sync::atomic::AtomicU32,
};

pub const MAX_SIZE: usize = 64; // needs tweaking
pub static DPI_VALUE: AtomicU32 = AtomicU32::new(250);
pub const BUFFER_SIZE: usize = 1024;

#[inline]
fn default_scale() -> f32 {
    1.
}

#[inline]
fn default_duration() -> f32 {
    0.5
}

#[inline]
fn default_perfect() -> u32 {
    0xe1ffec9f
}

#[inline]
fn default_good() -> u32 {
    0xebb4e1ff
}

#[inline]
fn default_tinted() -> bool {
    true
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResPackInfo {
    pub name: String,
    pub author: String,

    pub hit_fx: (u32, u32),
    #[serde(default = "default_duration")]
    pub hit_fx_duration: f32,
    #[serde(default = "default_scale")]
    pub hit_fx_scale: f32,
    #[serde(default)]
    pub hit_fx_rotate: bool,
    #[serde(default)]
    pub hide_particles: bool,
    #[serde(default = "default_tinted")]
    pub hit_fx_tinted: bool,

    pub hold_atlas: (u16, u16),
    #[serde(rename = "holdAtlasMH")]
    pub hold_atlas_mh: (u16, u16),

    #[serde(default)]
    pub hold_keep_head: bool,
    #[serde(default)]
    pub hold_repeat: bool,
    #[serde(default)]
    pub hold_compact: bool,

    #[serde(default = "default_perfect")]
    color_perfect: u32,
    #[serde(default = "default_good")]
    color_good: u32,

    #[serde(default)]
    pub description: String,
}

fn parse_color_guess_alpha(c: u32) -> Color {
    if c > 0xffffff {
        Color::from_hex_argb(c)
    } else {
        Color::from_hex_rgb(c)
    }
}

impl ResPackInfo {
    pub fn verify(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("empty name");
        }
        if self.name.len() > 100 {
            bail!("name too long");
        }
        if self.description.len() > 1000 {
            bail!("description too long");
        }
        if !(1..=10240).contains(&self.hit_fx.0.saturating_mul(self.hit_fx.1)) {
            bail!("Invalid hit_fx");
        }
        Ok(())
    }
    pub fn color_perfect(&self) -> Color {
        parse_color_guess_alpha(self.color_perfect)
    }

    pub fn color_good(&self) -> Color {
        parse_color_guess_alpha(self.color_good)
    }

    pub fn fx_perfect(&self) -> Color {
        if self.hit_fx_tinted {
            self.color_perfect()
        } else {
            WHITE
        }
    }

    pub fn fx_good(&self) -> Color {
        if self.hit_fx_tinted {
            self.color_good()
        } else {
            WHITE
        }
    }
}

pub struct NoteStyle {
    pub click: SafeTexture,
    pub hold: SafeTexture,
    pub flick: SafeTexture,
    pub drag: SafeTexture,
    pub hold_body: Option<SafeTexture>,
    pub hold_atlas: (u16, u16),
}

impl NoteStyle {
    pub fn verify(&self) -> Result<()> {
        if self.hold_atlas.0.saturating_add(self.hold_atlas.1) as f32 >= self.hold.height() {
            bail!("Invalid atlas");
        }
        Ok(())
    }

    #[inline]
    fn to_uv(&self, t: u16) -> f32 {
        t as f32 / self.hold.height()
    }

    pub fn hold_ratio(&self) -> f32 {
        self.hold.height() / self.hold.width()
    }

    pub fn hold_head_rect(&self) -> Rect {
        let sy = self.to_uv(self.hold_atlas.1);
        Rect::new(0., 1. - sy, 1., sy)
    }

    pub fn hold_body_rect(&self) -> Rect {
        let sy = self.to_uv(self.hold_atlas.0);
        let ey = 1. - self.to_uv(self.hold_atlas.1);
        Rect::new(0., sy, 1., ey - sy)
    }

    pub fn hold_tail_rect(&self) -> Rect {
        let ey = self.to_uv(self.hold_atlas.0);
        Rect::new(0., 0., 1., ey)
    }
}

pub struct ResourcePack {
    pub info: ResPackInfo,
    pub note_style: NoteStyle,
    pub note_style_mh: NoteStyle,
    pub sfx_click: AudioClip,
    pub sfx_drag: AudioClip,
    pub sfx_flick: AudioClip,
    pub ending: AudioClip,
    pub hit_fx: SafeTexture,
}

impl ResourcePack {
    pub async fn from_path<T: AsRef<Path>>(path: Option<T>) -> Result<Self> {
        Self::load(
            if let Some(path) = path {
                crate::fs::fs_from_file(path.as_ref())?
            } else {
                crate::fs::fs_from_assets("respack/")?
            }
            .deref_mut(),
        )
        .await
    }

    pub async fn load(fs: &mut dyn FileSystem) -> Result<Self> {
        macro_rules! load_tex {
            ($path:literal) => {
                SafeTexture::from(image::load_from_memory(&fs.load_file($path).await.with_context(|| format!("Missing {}", $path))?)?)
                    .with_filter(GL_LINEAR)
            };
        }
        let info: ResPackInfo = serde_yaml::from_str(&String::from_utf8(fs.load_file("info.yml").await.context("Missing info.yml")?)?)?;
        info.verify()?;
        let mut note_style = NoteStyle {
            click: load_tex!("click.png"),
            hold: load_tex!("hold.png"),
            flick: load_tex!("flick.png"),
            drag: load_tex!("drag.png"),
            hold_body: None,
            hold_atlas: info.hold_atlas,
        };
        note_style.verify()?;
        let mut note_style_mh = NoteStyle {
            click: load_tex!("click_mh.png"),
            hold: load_tex!("hold_mh.png"),
            flick: load_tex!("flick_mh.png"),
            drag: load_tex!("drag_mh.png"),
            hold_body: None,
            hold_atlas: info.hold_atlas_mh,
        };
        note_style_mh.verify()?;

        if info.hold_repeat {
            fn get_body(style: &mut NoteStyle) {
                let pixels = style.hold.get_texture_data();
                let width = style.hold.width() as u16;
                let height = style.hold.height() as u16;
                let atlas = style.hold_atlas;
                let res = Texture2D::from_rgba8(
                    width,
                    height - atlas.0 - atlas.1,
                    &pixels.bytes[(atlas.0 as usize * width as usize * 4)..(pixels.bytes.len() - atlas.1 as usize * width as usize * 4)],
                );
                let context = unsafe { get_internal_gl() }.quad_context;
                res.raw_miniquad_texture_handle().set_wrap(context, TextureWrap::Repeat);
                style.hold_body = Some(res.into());
            }
            get_body(&mut note_style);
            get_body(&mut note_style_mh);
        }
        let hit_fx = image::load_from_memory(&fs.load_file("hit_fx.png").await.context("Missing hit_fx.png")?)?.into();

        macro_rules! load_clip {
            ($path:literal) => {
                if let Some(sfx) = fs
                    .load_file(format!("{}.ogg", $path).as_str())
                    .await
                    .ok()
                    .map(|it| AudioClip::new(it))
                    .transpose()?
                {
                    sfx
                } else if let Some(sfx) = fs
                    .load_file(format!("{}.wav", $path).as_str())
                    .await
                    .ok()
                    .map(|it| AudioClip::new(it))
                    .transpose()?
                {
                    sfx
                } else if let Some(sfx) = fs
                    .load_file(format!("{}.mp3", $path).as_str())
                    .await
                    .ok()
                    .map(|it| AudioClip::new(it))
                    .transpose()?
                {
                    sfx
                } else {
                    AudioClip::new(load_file(format!("{}.ogg", $path).as_str()).await?)?
                }
            };
        }

        Ok(Self {
            info,
            note_style,
            note_style_mh,
            sfx_click: load_clip!("click"),
            sfx_drag: load_clip!("drag"),
            sfx_flick: load_clip!("flick"),
            ending: load_clip!("ending"),
            hit_fx,
        })
    }
}

pub struct ParticleEmitter {
    pub scale: f32,
    pub emitter: Emitter,
    pub emitter_square: Emitter,
    pub hide_particles: bool,
}

impl ParticleEmitter {
    pub fn new(res_pack: &ResourcePack, scale: f32, hide_particles: bool) -> Result<Self> {
        let colors_curve = {
            let start = WHITE;
            let mut mid = start;
            let mut end = start;
            mid.a *= 0.7;
            end.a = 0.;
            ColorCurve { start, mid, end }
        };
        let mut res = Self {
            scale: res_pack.info.hit_fx_scale,
            emitter: Emitter::new(EmitterConfig {
                local_coords: false,
                texture: Some(*res_pack.hit_fx),
                lifetime: res_pack.info.hit_fx_duration,
                lifetime_randomness: 0.0,
                initial_rotation_randomness: 0.0,
                initial_direction_spread: 0.0,
                initial_velocity: 0.0,
                atlas: Some(AtlasConfig::new(res_pack.info.hit_fx.0 as _, res_pack.info.hit_fx.1 as _, ..)),
                emitting: false,
                colors_curve,
                ..Default::default()
            }),
            emitter_square: Emitter::new(EmitterConfig {
                local_coords: false,
                lifetime: res_pack.info.hit_fx_duration,
                lifetime_randomness: 0.0,
                initial_direction_spread: 2. * std::f32::consts::PI,
                size_randomness: 0.3,
                emitting: false,
                initial_velocity: 2.5 * scale,
                initial_velocity_randomness: 1. / 10.,
                linear_accel: -6. / 1.,
                colors_curve,
                ..Default::default()
            }),
            hide_particles,
        };
        res.set_scale(scale);
        Ok(res)
    }

    pub fn emit_at(&mut self, pt: Vec2, rotation: f32, color: Color) {
        self.emitter.config.initial_rotation = rotation;
        self.emitter.config.base_color = color;
        self.emitter.emit(pt, 1);
        if !self.hide_particles {
            self.emitter_square.config.base_color = color;
            self.emitter_square.emit(pt, 4);
        }
    }

    pub fn draw(&mut self, dt: f32) {
        self.emitter.draw(vec2(0., 0.), dt);
        self.emitter_square.draw(vec2(0., 0.), dt);
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.emitter.config.size = self.scale * scale / 5.;
        self.emitter_square.config.size = self.scale * scale / 44.;
    }
}

type NoteBufferMap = BTreeMap<(i8, GLuint), Vec<(Vec<Vertex>, Vec<u16>)>>;

#[derive(Default)]
pub struct NoteBuffer(NoteBufferMap);

impl NoteBuffer {
    pub fn push(&mut self, key: (i8, GLuint), vertices: [Vertex; 4]) {
        let meshes = self.0.entry(key).or_default();
        if meshes.last().is_none_or(|it| it.0.len() + 4 > MAX_SIZE * 4) {
            meshes.push(Default::default());
        }
        let last = meshes.last_mut().unwrap();
        let i = last.0.len() as u16;
        last.0.extend_from_slice(&vertices);
        last.1.extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
    }

    pub fn draw_all(&mut self) {
        let mut gl = unsafe { get_internal_gl() };
        gl.flush();
        let gl = gl.quad_gl;
        gl.draw_mode(DrawMode::Triangles);
        for ((_, tex_id), meshes) in std::mem::take(&mut self.0).into_iter() {
            gl.texture(Some(Texture2D::from_miniquad_texture(unsafe { Texture::from_raw_id(tex_id, miniquad::TextureFormat::RGBA8) })));
            for mesh in meshes {
                gl.geometry(&mesh.0, &mesh.1);
            }
        }
    }
}

pub type SfxMap = HashMap<String, Sfx>;

pub struct Resource {
    pub config: Config,
    pub info: ChartInfo,
    pub aspect_ratio: f32,
    /// 谱面坐标 → 世界坐标的除数。与 `aspect_ratio`（世界 → 屏幕）分开，
    /// 竖屏「铺满不裁切」时两者不等。横屏恒相等。
    pub chart_aspect: f32,
    pub dpi: u32,
    pub last_vp: (i32, i32, i32, i32),
    /// 上一次的 (采样数, TAA 开, 需要离屏目标)。用于判断抗锯齿设置是否变了。
    last_aa: (u32, bool, bool),
    /// 上一次的渲染精度。变化时要重建离屏目标。
    last_scale: f32,
    pub note_width: f32,

    pub time: f64,

    pub alpha: f32,
    pub judge_line_color: Color,

    pub camera: Camera2D,

    pub background: SafeTexture,
    pub illustration: SafeTexture,
    pub icons: [SafeTexture; 8],
    pub mod_icons: [SafeTexture; 7],
    pub res_pack: ResourcePack,
    pub player: SafeTexture,
    pub icon_back: SafeTexture,
    pub icon_retry: SafeTexture,
    pub icon_resume: SafeTexture,
    pub icon_proceed: SafeTexture,

    pub emitter: ParticleEmitter,

    pub audio: AudioManager,
    pub music: AudioClip,
    pub track_length: f64,
    pub sfx_click: Sfx,
    pub sfx_drag: Sfx,
    pub sfx_flick: Sfx,

    pub extra_sfxs: SfxMap,

    pub chart_target: Option<MSRenderTarget>,
    /// 【话梅】高质量空间上采样（bicubic）。懒初始化（建 shader 要 GL 上下文）。
    /// 放在 Resource 里而不是 GameScene：blit 时要和 `chart_target` 同时可变借用，
    /// 同结构体不同字段才合法。
    pub upscaler: Option<crate::upscale::Upscaler>,
    pub no_effect: bool,

    pub note_buffer: RefCell<NoteBuffer>,

    pub model_stack: Vec<Matrix>,
}

macro_rules! loads {
    ($($path:literal),*) => {
        [$(loads!(@detail $path)),*]
    };

    (@detail $path:literal) => {
        Texture2D::from_image(&load_image($path).await?).into()
    };
}

/// 由视口尺寸算出 `(chart_aspect, view_aspect)`。
///
/// 纯函数：不碰任何状态，便于单测与推演「拉高窗口会怎样」。
///
/// 【密度基准 = 短边】
///   每个谱面单位占 `短边 / 2` 像素 —— 也就是「把设备横过来时谱高的尺寸」。
///     · 横屏：短边是高 → 谱面单位 = 半屏高（0.8.2 以来一致）
///     · 竖屏：短边是宽 → 谱面单位 = 半屏宽（与横屏同一密度）
///   所以竖屏拉高**只延长可视范围，不改变音符大小**。
///
/// 【可视范围】
///   `chart_aspect / view_aspect`，即判定线以上能看到多少个谱面单位：
///     · 横屏：恒为 1.0
///     · 竖屏：等于 `高 / 宽`，随高度线性变化
///
/// ⚠ 曾经写成 `min(design, 1/screen_ar)`：那等于把密度基准取成对角线方向，
///   窗口一高音符就被压小，看起来像「拉长了」，其实是单位变小。
pub fn aspects_for(vp_w: f32, vp_h: f32, design: f32) -> (f32, f32) {
    let screen_ar = vp_w / vp_h;
    /* w / min(w,h)：横屏 = w/h，竖屏 = 1。再被谱面设计比例封顶。 */
    let density_base = vp_w / vp_w.min(vp_h);
    let chart_ar = design.min(density_base);
    /* 横屏沿用 0.8.2 的 letterbox 行为；竖屏铺满。 */
    let view_ar = if screen_ar >= 1.0 { chart_ar } else { screen_ar };
    (chart_ar, view_ar)
}

impl Resource {
    pub async fn load_icons() -> Result<[SafeTexture; 8]> {
        Ok(loads![
            "rank/F.png",
            "rank/C.png",
            "rank/B.png",
            "rank/A.png",
            "rank/S.png",
            "rank/V.png",
            "rank/FC.png",
            "rank/phi.png"
        ])
    }
    pub async fn load_mod_icons() -> Result<[SafeTexture; 7]> {
        // FLIP_X, FADE_OUT, FADE_IN, NIGHTCORE, RAINBOW, AUTOPLAY, NO_SHADER
        Ok(loads![
            "mod/flip_x.png",
            "mod/fade_out.png",
            "mod/fade_in.png",
            "mod/nightcore.png",
            "mod/rainbow.png",
            "mod/autoplay.png",
            "mod/no-shader.png"
        ])
    }

    pub async fn new(
        config: Config,
        info: ChartInfo,
        mut fs: Box<dyn FileSystem>,
        player: Option<SafeTexture>,
        background: SafeTexture,
        illustration: SafeTexture,
        has_no_effect: bool,
    ) -> Result<Self> {
        macro_rules! load_tex {
            ($path:literal) => {
                SafeTexture::from(Texture2D::from_image(&load_image($path).await?))
            };
        }
        let res_pack = ResourcePack::from_path(config.res_pack_path.as_ref())
            .await
            .context("Failed to load resource pack")?;
        let camera = Camera2D {
            target: vec2(0., 0.),
            zoom: vec2(1., -config.aspect_ratio.unwrap_or(info.aspect_ratio)),
            ..Default::default()
        };

        let mut audio = create_audio_manger(&config)?;
        let music = AudioClip::new(
            fs.load_file(&info.music)
                .await
                .with_context(|| format!("load music {}", info.music))?,
        )?;
        let track_length = music.length();
        let buffer_size = Some(BUFFER_SIZE);
        let sfx_click = audio.create_sfx(res_pack.sfx_click.clone(), buffer_size)?;
        let sfx_drag = audio.create_sfx(res_pack.sfx_drag.clone(), buffer_size)?;
        let sfx_flick = audio.create_sfx(res_pack.sfx_flick.clone(), buffer_size)?;

        let aspect_ratio = config.aspect_ratio.unwrap_or(info.aspect_ratio);
        let note_width = config.note_scale * NOTE_WIDTH_RATIO_BASE as f32;
        let note_scale = config.note_scale;

        let emitter = ParticleEmitter::new(&res_pack, note_scale, res_pack.info.hide_particles)?;

        /* 低画质模式也走这条：关特效 + 不进 MSAA 目标（省一次全屏 blit）。
           ⚠ 它和 `Mods::NO_SHADER` 的区别：那种是 mod，会进 UNRATED、不算成绩；
           这里是纯画质开关，成绩照常计入、照常上传。 */
        let no_effect = config.disable_effect || config.low_quality || has_no_effect;

        macroquad::window::gl_set_drawcall_buffer_capacity(MAX_SIZE * 4, MAX_SIZE * 6);
        Ok(Self {
            config,
            info,
            aspect_ratio,
            /* 开局时两者相同；第一次 update_size 会按屏幕方向重新分配。 */
            chart_aspect: aspect_ratio,
            dpi: DPI_VALUE.load(std::sync::atomic::Ordering::SeqCst),
            last_vp: (0, 0, 0, 0),
            last_aa: (0, false, false),
            last_scale: 1.,
            note_width,

            time: 0.,

            alpha: 1.,
            judge_line_color: res_pack.info.fx_perfect(),

            camera,

            background,
            illustration,
            icons: Self::load_icons().await?,
            mod_icons: Self::load_mod_icons().await?,
            res_pack,
            player: if let Some(player) = player { player } else { load_tex!("player.jpg") },
            icon_back: load_tex!("back.png"),
            icon_retry: load_tex!("retry.png").with_mipmap(),
            icon_resume: load_tex!("resume.png"),
            icon_proceed: load_tex!("proceed.png").with_mipmap(),

            emitter,

            audio,
            music,
            track_length,
            sfx_click,
            sfx_drag,
            sfx_flick,
            extra_sfxs: SfxMap::new(),

            chart_target: None,
            upscaler: None,
            no_effect,

            note_buffer: RefCell::new(NoteBuffer::default()),

            model_stack: vec![Matrix::identity()],
        })
    }

    pub fn create_sfx(&mut self, clip: AudioClip) -> Result<Sfx> {
        self.audio.create_sfx(clip, Some(BUFFER_SIZE))
    }

    pub fn emit_at_origin(&mut self, rotation: f32, color: Color) {
        if !self.config.particle {
            return;
        }
        let pt = self.world_to_screen(Point::default());
        self.emitter.emit_at(
            vec2(if self.config.flip_x() { -pt.x } else { pt.x }, -pt.y),
            if self.res_pack.info.hit_fx_rotate { rotation.to_radians() } else { 0. },
            color,
        );
    }

    pub fn update_size(&mut self, vp: (i32, i32, i32, i32)) -> bool {
        /* 【话梅 2026-11-16】抗锯齿切换必须**重建** chart_target。
           `MSRenderTarget` 的采样数在创建时就固定了（glRenderbufferStorageMultisample），
           运行期改 sample_count 不会改变它 —— 只改配置不重建，就会拿
           「4x 的 FBO」按「1x 的假设」去 blit，或者反过来：
           在 GL 上表现为 `glBlitFramebuffer` 尺寸/采样不匹配 → **画面卡死/黑屏**。
           这正是一改抗锯齿就卡住的原因。

           原来这里只在 `last_vp != vp` 时重建 —— 改设置不改窗口，条件永远不成立。
           现在把 (采样数, TAA, 是否要离屏) 一起纳入「是否需要重建」的判断。 */
        let want_target = !self.no_effect || self.config.sample_count != 1 || self.config.taa;
        /* 渲染精度也算进「重建条件」—— 目标尺寸变了必须重建，否则贴图拉伸会糊/错位。 */
        let scale = if self.config.render_scale.is_finite() { self.config.render_scale.clamp(0.6, 1.) } else { 1. };
        let aa_now = (self.config.sample_count.max(1), self.config.taa, want_target);
        let scale_changed = (self.last_scale - scale).abs() > 1e-4;
        self.last_scale = scale;
        let aa_changed = self.last_aa != aa_now;
        if self.last_vp == vp && !aa_changed && !scale_changed {
            return false;
        }
        self.last_vp = vp;
        self.last_aa = aa_now;
        if want_target {
            /* TAA 也要离屏目标 —— 它需要上一帧纹理做时间混合；
               此时 sample_count 被 UI 强制为 1，所以不付 MSAA 的带宽。 */
            /* 渲染精度：离屏目标按比例缩小，blit 时再放大铺满 —— 这就是分辨率缩放。
               下限 1 像素，避免 scale 极端时建出 0 尺寸 FBO。 */
            let tw = ((vp.2 as f32 * scale).round() as u32).max(1);
            let th = ((vp.3 as f32 * scale).round() as u32).max(1);
            self.chart_target = Some(MSRenderTarget::new((tw, th), aa_now.0));
        } else {
            /* 从「有离屏」切回「无离屏」时也要清掉，否则会继续用旧的 target 贴图。 */
            self.chart_target = None;
        }
        fn viewport(aspect_ratio: f32, (x, y, w, h): (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
            let w = w as f32;
            let h = h as f32;
            let (rw, rh) = {
                let ew = h * aspect_ratio;
                if ew > w {
                    let eh = w / aspect_ratio;
                    (w, eh)
                } else {
                    (ew, h)
                }
            };
            (x + ((w - rw) / 2.).round() as i32, y + ((h - rh) / 2.).round() as i32, rw as i32, rh as i32)
        }
        let chart_ar = self.config.aspect_ratio.unwrap_or(self.info.aspect_ratio);

        /* ── 竖屏铺满：把「谱面比例」和「视图比例」分开（2026-11-15 重构）──
         *
         * 【为什么必须拆成两个】
         *   同一个 `aspect_ratio` 过去同时干两件事：
         *     · 谱面坐标 → 世界坐标的除数（对象位移 / 音符高度 / 判定线高度）
         *     · 世界坐标 → 屏幕的相机缩放
         *   因为两者恒等，所以看不出问题。但竖屏要「铺满、不裁切」，就只能
         *   二选一：
         *     - 相机跟着屏幕 → 世界 x/y 不等比，圆形音符被拉成椭圆
         *     - 谱面锁死比例 → 上下黑边（旧行为，就是截图里那条）
         *   拆开之后两个都能要。
         *
         * 【取值】
         *   chart_ar = 这台设备**横过来时**会用的谱面比例
         *              （iPad 4:3 机器：min(1.78, 1.43) = 1.43，与横屏完全一致）
         *   view_ar  = 竖屏取屏幕自身比例（铺满）；横屏沿用 0.8.2 以来
         *              的 min(谱面, 屏幕)，一行行为都不变
         *
         * 【竖屏的收益】
         *   谱面密度（音符像素大小、上下间距对应的谱面距离）与横屏一致，
         *   但屏幕更高 ⇒ 同样的谱面范围铺到更多像素上，能看到更远的音符。
         *   iPad Pro 11" 竖屏实测：可视谱面范围 1.0 → 2.04，预读时间翻倍。
         *
         * 【横屏零回归】
         *   screen_ar >= 1 时 chart_ar == view_ar == 旧 effective，逐字节一致。 */
        /* 密度基准 = **短边**。
         *
         * 每个谱面单位占多少像素，应当等于「短边 / 2」—— 也就是把设备
         * 横过来时谱面高度的那个尺寸。这样：
         *   · 横屏：短边是高，谱面单位 = 半屏高（0.8.2 以来的表现，不变）
         *   · 竖屏：短边是宽，谱面单位 = 半屏宽（= 横屏时的同一密度）
         *
         * 于是竖屏**拉高窗口只会延长可视范围，不会改变音符大小** ——
         * 渲染距离 = 高度 / 宽度，随高度连续变化。这才是竖屏该有的行为。
         *
         * ⚠ 曾经写成 chart_ar = min(design, 1/screen_ar)：等价于把密度基准
         *   取成了「屏幕对角线方向的比例」，窗口一高音符就被压小，
         *   看起来像"拉长了"，其实是每个单位变小了。 */
        let (chart_ar, view_ar) = aspects_for(vp.2 as f32, vp.3 as f32, chart_ar);

        self.chart_aspect = chart_ar;
        self.aspect_ratio = view_ar;
        self.camera.zoom.y = -view_ar;
        self.camera.viewport = Some(viewport(view_ar, vp));
        /* 一行诊断：把「算出来的视口」和「实际屏幕」一起打出去。
           竖屏疑似「上下各空一截」时，看这一行就能判断是 view_ar 不对、
           还是相机视口被 letterbox 了 —— 不用再猜。
           iOS 上走 Documents/motion.log（NSLog 没有 Mac 读不到）。 */
        crate::huamei::diag(format!(
            "viewport: screen={}x{} chart_ar={:.3} view_ar={:.3} 可视范围={:.2} cam_vp={:?} dpi={}",
            vp.2,
            vp.3,
            chart_ar,
            view_ar,
            chart_ar / view_ar,
            self.camera.viewport,
            self.dpi
        ));
        true
    }

    pub fn world_to_screen(&self, pt: Point) -> Point {
        self.model_stack.last().unwrap().transform_point(&pt)
    }

    pub fn screen_to_world(&self, pt: Point) -> Point {
        self.model_stack.last().unwrap().try_inverse().unwrap().transform_point(&pt)
    }

    #[inline]
    pub fn with_model(&mut self, model: Matrix, f: impl FnOnce(&mut Self)) {
        let model = self.model_stack.last().unwrap() * model;
        self.model_stack.push(model);
        f(self);
        self.model_stack.pop();
    }

    #[inline]
    pub fn apply_model(&mut self, f: impl FnOnce(&mut Self)) {
        self.apply_model_of(&self.model_stack.last().unwrap().clone(), f);
    }

    #[inline]
    pub fn apply_model_of(&mut self, mat: &Matrix, f: impl FnOnce(&mut Self)) {
        unsafe { get_internal_gl() }.quad_gl.push_model_matrix(nalgebra_to_glm(mat));
        f(self);
        unsafe { get_internal_gl() }.quad_gl.pop_model_matrix();
    }
}

#[cfg(test)]
mod aspect_tests {
    use super::aspects_for;

    /// 判定线以上能看到多少个谱面单位（这就是「渲染距离」）。
    fn visible(w: f32, h: f32, design: f32) -> f32 {
        let (c, v) = aspects_for(w, h, design);
        c / v
    }

    /// 每个谱面单位占多少像素。
    /// 世界坐标横跨 ±1 铺满宽度 → 每世界单位 `w/2` 像素；
    /// 谱面单位 → 世界单位要除以 `chart_aspect`。
    fn px_per_unit(w: f32, h: f32, design: f32) -> f32 {
        let (c, _) = aspects_for(w, h, design);
        (w / 2.) / c
    }

    /* ── 横屏：与 0.8.2 完全一致 ── */
    #[test]
    fn landscape_unchanged() {
        // iPad Pro 11" 横屏 1194×834，谱面设计 16:9
        let (c, v) = aspects_for(1194., 834., 1.7778);
        assert!((c - 1194. / 834.).abs() < 1e-4, "chart_ar={c}");
        assert!((v - c).abs() < 1e-6, "横屏两者必须相等（否则视口会错位）");
        assert!((visible(1194., 834., 1.7778) - 1.0).abs() < 1e-6);
    }

    /// 更宽的屏（21:9）仍然按谱面设计比例 letterbox，行为不变。
    #[test]
    fn landscape_ultrawide_letterboxes() {
        let (c, v) = aspects_for(2560., 1080., 1.7778);
        assert!((c - 1.7778).abs() < 1e-4, "chart_ar={c}");
        assert!((v - c).abs() < 1e-6);
    }

    /* ── 竖屏：渲染距离随高度变化，音符大小不变 ── */
    #[test]
    fn portrait_density_is_width_based() {
        // 同一宽度、三种高度：每单位像素必须完全一致
        let w = 834.;
        let base = px_per_unit(w, 1194., 1.7778);
        /* 只测竖屏（h > w）：横屏的密度基准是高度，本就该等于 h/2。 */
        for h in [1000., 1194., 1600., 2400.] {
            let p = px_per_unit(w, h, 1.7778);
            assert!((p - base).abs() < 1e-3, "h={h} 像素密度变了：{p} vs {base}");
        }
    }

    #[test]
    fn portrait_render_distance_scales_with_height() {
        let w = 834.;
        let mut last = 0.;
        for h in [1000., 1400., 2000., 2800.] {
            let vis = visible(w, h, 1.7778);
            /* 可视范围 = 高 / 宽 —— 线性于高度 */
            assert!((vis - h / w).abs() < 1e-4, "h={h} 可视范围 {vis}，期望 {}", h / w);
            assert!(vis > last, "拉高窗口后可视范围必须变大");
            last = vis;
        }
    }

    /// 竖屏要比横屏看得远（这才是「竖屏优势」）。
    #[test]
    fn portrait_sees_further_than_landscape() {
        let landscape = visible(1194., 834., 1.7778);
        let portrait = visible(834., 1194., 1.7778);
        assert!(portrait > landscape * 1.4, "竖屏 {portrait} 相对横屏 {landscape} 优势不足");
    }

    /// 极端形状不能算出 0 / 负数 —— 否则相机缩放会炸。
    #[test]
    fn degenerate_sizes_are_safe() {
        for (w, h) in [(1., 3000.), (3000., 1.), (500., 500.)] {
            let (c, v) = aspects_for(w, h, 1.7778);
            assert!(c > 0. && v > 0., "w={w} h={h} -> {c},{v}");
            assert!(c.is_finite() && v.is_finite());
        }
    }
}
