pub mod bin;
/// 【话梅 2026-11-18】从 PhiraiAd 移植：每谱的自动倒打区间 + 按键转换设置
pub mod chart_play;
/// 倒打播放状态机（旋转动画、区间调度）
pub mod auto_flip;
pub mod config;
pub mod core;
pub mod dir;
pub mod ext;
pub mod fs;
/// 【huamei 新增】数据飞轮的对局记录格式与紧凑编码。
/// 契约见 `docs/规范/数据飞轮契约.md`。
pub mod huamei;
pub mod info;
pub mod judge;
pub mod parse;
pub mod particle;
/* 【话梅 ← PhiraiAd】练习辅助：流速曲线 + 视野控制。
   两个模块都是零 crate 内依赖的纯函数，先独立引入，
   后续再逐步接进 config / game UI。 */
pub mod practice_speed;
pub mod practice_view;
pub mod scene;
pub mod task;
/// 【话梅】时间抗锯齿（TAA）
pub mod taa;
/// 【话梅】高质量空间上采样（MetalFX Spatial 的 GLES 等效实现）
pub mod upscale;
/// 【话梅】判定范围可视化（参考 PhiraiAd，用常量窗口重写）
pub mod judgement_range;
/// 【话梅】RKS 计算（官方 Phira 口径：单曲公式 + R10+B20）
pub mod rks;
/// 【话梅】实时判定条（参考 PhiraiAd，自包含重写）
pub mod timing_bar;
pub mod time;
pub mod ui;

#[cfg(feature = "log")]
pub mod log;

#[rustfmt::skip]
#[cfg(closed)]
pub mod inner;

pub use scene::Main;

pub fn build_conf() -> macroquad::window::Conf {
    /* 桌面窗口尺寸可用环境变量覆盖 —— 竖屏适配没有真机时，这是唯一的
       验证手段：`PHM_WIN_W=608 PHM_WIN_H=973 phira-main` 就能看竖屏。
       不设就是原来的 973×608。 */
    let env_i32 = |k: &str, def: i32| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.trim().parse::<i32>().ok())
            .filter(|v| (200..=8000).contains(v))
            .unwrap_or(def)
    };
    macroquad::window::Conf {
        window_title: "Phira".to_string(),
        window_width: env_i32("PHM_WIN_W", 973),
        window_height: env_i32("PHM_WIN_H", 608),
        ..Default::default()
    }
}
