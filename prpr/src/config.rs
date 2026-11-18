//! Configuration module of the playing environment.\
//! e.g. player name, volume, speed, autoplay, etc.

use bitflags::bitflags;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// serde 默认 true —— `#[serde(default)]` 对 bool 是 false，
/// 那会让**所有旧存档**在升级后自动变成「关闭上报」，不是我们想要的默认。
fn default_true() -> bool {
    true
}

pub static TIPS: Lazy<Vec<String>> = Lazy::new(|| include_str!("tips.txt").split('\n').map(str::to_owned).collect());

bitflags! {
    #[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, Debug)]
    #[serde(transparent)]
    pub struct Mods: i32 {
        const AUTOPLAY = 0x0001;
        const FLIP_X = 0x0002;
        const FADE_OUT = 0x0004;
        const FADE_IN = 0x0008;
        const NIGHTCORE = 0x0010;
        const RAINBOW = 0x0020;
        const NO_SHADER = 0x0040;
        const INSTANT_DEATH_AP = 0x0080;
        const INSTANT_DEATH_FC = 0x0100;

        const UNRATED = Self::AUTOPLAY.bits() | Self::NO_SHADER.bits();
    }
}

impl Mods {
    pub fn toggle_mod(&mut self, flag: Mods) {
        if self.contains(flag) {
            self.remove(flag);
        } else {
            for &conflict in Mods::conflicts(flag) {
                self.remove(conflict);
            }
            self.insert(flag);
        }
    }
    fn conflicts(flag: Mods) -> &'static [Mods] {
        match flag {
            /* 【2026-11-15 移除】FADE_IN 与 FADE_OUT 原本互斥。
               放开是为了支持「全隐」：两个都开时 note.rs 取 min，
               形成中间淡出、两端可见的沙漏。issue #785。 */
            Mods::INSTANT_DEATH_AP => &[Mods::INSTANT_DEATH_FC],
            Mods::INSTANT_DEATH_FC => &[Mods::INSTANT_DEATH_AP],
            _ => &[],
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(rename = "adjust_time_new")]
    pub adjust_time: bool,
    pub aggressive: bool,
    pub ap_fc_indicator: bool,
    pub aspect_ratio: Option<f32>,
    pub audio_buffer_size: Option<u32>,
    pub chart_debug: bool,
    pub disable_effect: bool,
    pub double_click_to_pause: bool,
    pub double_hint: bool,
    pub fullscreen_mode: bool,
    pub fxaa: bool,
    pub interactive: bool,
    pub mods: Mods,
    pub mp_address: String,
    /// 【huamei】低画质模式。
    ///
    /// ⚠ 和 `Mods::NO_SHADER` **不是一回事**：那是「mod」，被算进 UNRATED，
    ///   会让这一局不计成绩、不上传（game.rs 的上报守卫会拦）。
    ///   这里是纯画质设置 —— 关特效 / 关粒子 / 60fps，**不影响成绩有效性**。
    ///   想省电又想要成绩的人就该用这个。
    #[serde(default)]
    pub low_quality: bool,
    pub mp_enabled: bool,
    pub note_scale: f32,
    /// 【话梅 2026-11-18】全局**视觉**流速倍率（0.1~5）。
    ///
    /// 与 `speed` 的区别（上游注释写得很明白）：
    ///   · `speed` 改的是谱面时间轴 —— 影响音符到达时刻与判定窗口
    ///   · `note_flow_speed` 只改**看起来下落多快**，不碰时间、音频、判定
    /// 所以玩家可以"视觉上放慢"来读谱，成绩仍然有效。
    ///
    /// 默认 1.0 = 与 speed 的表现完全一致（不引入任何变化）。
    pub note_flow_speed: f32,
    pub offline_mode: bool,
    pub offset: f32,
    pub particle: bool,
    pub player_name: String,
    pub player_rks: f32,
    pub preferred_sample_rate: Option<u32>,
    pub res_pack_path: Option<String>,
    pub sample_count: u32,
    /* 【话梅 2026-11-17 主页装扮】
       素材来自资源包：值是**资源包目录下的相对路径**（如 "home/bg.png"）。
       None = 用默认外观。
       存在 Config 而不是 Data：它和主题、字体一样属于「外观偏好」，
       跟着设置一起导出/同步更合理。 */
    /// 主页背景图（资源包内相对路径）
    #[serde(default)]
    pub home_bg: Option<String>,
    /// 主页背景音乐（资源包内相对路径）
    #[serde(default)]
    pub home_bgm: Option<String>,

    /* 【话梅 2026-11-17】搜索翻译服务地址（可选）。
       ─────────────────────────────────────────────────────────
       留空 = 走我们服务端的 /api/hm/translate（如果能用）。
       填了 = 直接请求用户自己部署的翻译服务，格式按 **LibreTranslate**：
            POST {endpoint}/translate
            {"q":"文本","source":"zh","target":"ja","format":"text"}
            → {"translatedText":"..."}

       为什么支持本地部署：翻译源在国内不好找（Google 直连 000），
       而 LibreTranslate 是开源自托管、一条 docker 命令就能跑。
       我们不需要为此维护任何 key，也不需要替用户承担翻译服务的成本。

       地址示例：
       · 电脑上跑、iOS 走局域网：http://192.168.1.10:5000
       · 同设备：http://127.0.0.1:5000 */
    #[serde(default)]
    pub translate_endpoint: Option<String>,

    /// 【话梅】判定范围可视化（调试用）。
    ///
    /// 对每个未判定的音符，把它在判定窗口起止时刻的位置差画成矩形：
    /// 青色 = Perfect 区、绿色 = Good 区、橙色 = Bad 区。
    /// 纯只读叠加，不参与判定、不影响成绩。
    #[serde(default)]
    pub show_judgement_range: bool,
    /// 【话梅】成绩上报（遥测）总开关。
    ///
    /// 合规要求（《个人信息保护法》第 15/16 条）：处理个人信息应取得同意，
    /// 并提供**便捷的拒绝方式**。采集内容含位置（敏感个人信息）与陀螺仪，
    /// 因此必须可关。
    ///
    /// 关闭后：**不再上报任何游玩数据**（成绩、判定、触摸热区、设备、位置、陀螺仪）。
    /// 已有的服务端记录不受影响，用户可另行申请删除。
    #[serde(default = "default_true")]
    pub telemetry_enabled: bool,
    /// 【话梅】渲染精度（0.6~1.0）。离屏目标按此比例缩放，最后放大铺满屏幕。
    /// 1.0 = 原生分辨率；越低越省填充率，画面越软。0 视为 1.0（旧存档兼容）。
    #[serde(default)]
    pub render_scale: f32,
    /// 【话梅】帧率上限。0 = 不限制；60 / 90 / 120 为显式上限。
    /// iOS 上走 `set_preferred_fps`，由系统按 CADisplayLink 节流。
    #[serde(default)]
    pub fps_limit: u32,
    /// 【话梅】TAA（时间抗锯齿）。与 MSAA 互斥：开 TAA 时 sample_count 强制为 1。
    /// 通过相邻帧混合消除边缘闪烁；音符高速移动时可能有轻微拖影。
    #[serde(default)]
    pub taa: bool,
    pub show_acc: bool,
    /// 【话梅】对局中显示实时判定条（默认关，设置→调试里开）。
    #[serde(default)]
    pub show_timing_bar: bool,
    pub show_avg_fps: bool,
    pub speed: f32,
    pub touch_debug: bool,
    pub use_keyboard: bool,
    pub volume_bgm: f32,
    pub volume_music: f32,
    pub volume_sfx: f32,

    // for compatibility
    autoplay: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            adjust_time: false,
            aggressive: true,
            ap_fc_indicator: true,
            aspect_ratio: None,
            audio_buffer_size: None,
            chart_debug: false,
            disable_effect: false,
            double_click_to_pause: true,
            double_hint: true,
            fxaa: false,
            interactive: true,
            mods: Mods::default(),
            mp_address: "mp2.phira.cn:12345".to_owned(),
            low_quality: false,
            mp_enabled: false,
            note_scale: 1.0,
            note_flow_speed: 1.0,
            offline_mode: false,
            fullscreen_mode: false,
            offset: 0.,
            particle: true,
            player_name: "Mivik".to_string(),
            player_rks: 15.,
            preferred_sample_rate: None,
            res_pack_path: None,
            sample_count: 1,
            taa: false,
            telemetry_enabled: true,
            show_judgement_range: false,
            home_bg: None,
            home_bgm: None,
            translate_endpoint: None,
            render_scale: 1.,
            fps_limit: 0,
            show_acc: false,
            show_timing_bar: true,
            show_avg_fps: false,
            speed: 1.,
            touch_debug: false,
            use_keyboard: false,
            volume_music: 1.,
            volume_sfx: 1.,
            volume_bgm: 1.,

            autoplay: None,
        }
    }
}

impl Config {
    pub fn init(&mut self) {
        /* 渲染精度：0 / 非有限 = 旧存档没这个字段，按原生处理 */
        if !self.render_scale.is_finite() || self.render_scale <= 0. {
            self.render_scale = 1.;
        }
        self.render_scale = self.render_scale.clamp(0.6, 1.);
        /* 帧率上限只接受白名单值，脏数据按「不限制」处理 */
        if !matches!(self.fps_limit, 0 | 60 | 90 | 120) {
            self.fps_limit = 0;
        }
        if let Some(flag) = self.autoplay {
            self.mods.set(Mods::AUTOPLAY, flag);
        }
        #[cfg(target_env = "ohos")]
        {
            // Due to the fucking poor performance of the Maloon GPU, the sample count must be set to 1.
            self.sample_count = 1;
        }
    }

    #[inline]
    pub fn has_mod(&self, m: Mods) -> bool {
        self.mods.contains(m)
    }

    #[inline]
    pub fn autoplay(&self) -> bool {
        self.has_mod(Mods::AUTOPLAY)
    }

    #[inline]
    /// 视觉流速，做了有限性校验与范围钳制。
    ///
    /// 为什么要 clamp 而不是直接返回：这个值会被乘进音符的屏幕位移，
    /// NaN/0/负数会让音符瞬移或消失。旧存档里如果存过坏值，
    /// 这里兜住比在渲染层崩掉好。
    pub fn global_note_flow_speed(&self) -> f32 {
        if self.note_flow_speed.is_finite() {
            self.note_flow_speed.clamp(0.1, 5.)
        } else {
            1.
        }
    }

    pub fn flip_x(&self) -> bool {
        self.has_mod(Mods::FLIP_X)
    }
}
