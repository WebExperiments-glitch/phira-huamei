pub mod bin;
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
pub mod scene;
pub mod task;
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
