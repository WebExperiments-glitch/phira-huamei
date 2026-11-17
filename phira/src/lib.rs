prpr_l10n::tl_file!("common" ttl crate::);

#[rustfmt::skip]
#[cfg(closed)]
mod inner;

mod anim;
/// 【huamei 新增】内嵌的 assets 清单（由 `huamei-tools/gen-embedded-assets.py` 生成）。
/// 让 exe 可以单文件分发 —— 见 `ensure_embedded_assets()`。
mod assets_embedded;
mod censor;
mod charts_view;
mod client;
mod data;
mod device;
/// 【话梅】主页装扮素材导入（文件/相册 → 资源包）
mod home_picker;
/// 【话梅】搜索查询的多语言扩展（中文 → 日/英）
mod translate;
mod location;
mod motion;
pub mod deeplink;
/// 【huamei 新增】huamei 账号登录 + 成绩上报。
/// 契约见 `docs/规范/数据飞轮契约.md`。
pub mod huamei;
/// 【话梅】定数后到时，回填历史里的估算值。
///
/// 第一次打一张新谱时缓存是空的，只能先用难度标签估算（"IN.16" -> 16.0）；
/// 等 PHM 的权威定数拿到后，把这张谱的**所有**历史记录改成权威值 ——
/// 这样最终结果与「一开始就有定数」完全一致，不会因为首次游玩而固化误差。
///
/// 只改 `difficulty`，不动 `acc` / `score` / `played_at`：
/// 那三项是既成事实，不该被追溯修改。
fn backfill_difficulty(chart_id: i32, difficulty: f32) {
    let data = get_data_mut();
    let mut changed = 0usize;
    for e in data.score_history.iter_mut() {
        if e.chart_id == chart_id && (e.difficulty - difficulty).abs() > 1e-4 {
            e.difficulty = difficulty;
            changed += 1;
        }
    }
    if changed > 0 {
        telem::note(format!("difficulty: 回填 {chart_id} 的 {changed} 条历史 -> {difficulty:.2}"));
        let _ = save_data();
    }
}

/// 【话梅】谱面定数客户端（PHM 开放平台）
mod difficulty;
/// 【话梅】线程 QoS 分级（大小核调度）
mod qos;
mod icons;
mod images;
mod login;
mod mp;
mod page;
mod popup;
mod rate;
mod resource;
mod scene;
mod tabs;
mod tags;
/// 诊断日志（Documents 落盘）。`pub` 是因为 `phira-main` 的 panic hook
/// 也要走同一套路径策略 —— 两份实现必然分叉。
pub mod telem;
mod threed;
mod uml;

use anyhow::Result;
use data::Data;
use macroquad::prelude::*;
use prpr::{
    build_conf,
    core::{init_assets, PGR_FONT},
    ext::SafeTexture,
    log,
    scene::show_error,
    time::TimeManager,
    ui::{cleanup_audio, FontArc, TextPainter},
    Main,
};
use prpr_l10n::set_prefered_locale;
#[cfg(not(feature = "hykb"))]
use prpr_l10n::{GLOBAL, LANGS};
use scene::MainScene;
use std::{
    collections::VecDeque,
    sync::{mpsc, Mutex},
};
use tracing::{error, info};

#[cfg(target_os = "android")]
use jni::{
    objects::{JClass, JString},
    sys::jint,
    EnvUnowned,
};

static MESSAGES_TX: Mutex<Option<mpsc::Sender<bool>>> = Mutex::new(None);
static DATA_PATH: Mutex<Option<String>> = Mutex::new(None);
static CACHE_DIR: Mutex<Option<String>> = Mutex::new(None);
pub static mut DATA: Option<Data> = None;

#[cfg(target_env = "ohos")]
use napi_derive_ohos::napi;

#[cfg(closed)]
pub async fn load_res(name: &str) -> Vec<u8> {
    let bytes = load_file(name).await.unwrap();
    inner::resolve_data(bytes)
}

#[allow(unused)]
pub async fn load_res_tex(name: &str) -> SafeTexture {
    #[cfg(closed)]
    {
        let bytes = load_res(name).await;
        let image = image::load_from_memory(&bytes).unwrap();
        image.into()
    }
    #[cfg(not(closed))]
    prpr::ext::BLACK_TEXTURE.clone()
}

pub fn sync_data() {
    if get_data().language.is_none() {
        #[cfg(feature = "hykb")]
        let default_lang = "zh-CN".to_owned();
        #[cfg(not(feature = "hykb"))]
        let default_lang = LANGS[GLOBAL.order.lock().unwrap()[0]].to_owned();
        get_data_mut().language = Some(default_lang);
    }
    set_prefered_locale(get_data().language.as_ref().and_then(|it| it.parse().ok()));
    let _ = client::set_access_token_sync(get_data().tokens.as_ref().map(|it| &*it.0));
}

pub fn set_data(data: Data) {
    unsafe {
        DATA = Some(data);
    }
}

#[allow(static_mut_refs)]
pub fn get_data() -> &'static Data {
    unsafe { DATA.as_ref().unwrap() }
}

#[allow(static_mut_refs)]
pub fn get_data_mut() -> &'static mut Data {
    unsafe { DATA.as_mut().unwrap() }
}

pub fn save_data() -> Result<()> {
    std::fs::write(format!("{}/data.json", dir::root()?), serde_json::to_string(get_data())?)?;
    Ok(())
}

/// 【huamei 新增】首次运行时把内嵌的 assets 释放到 exe 旁边，让 exe 能**单文件分发**。
///
/// 【为什么必须落盘，而不是在内存里读】
///   Phira 通过 macroquad 的 `load_file("assets/...")` 读资源，而这个
///   macroquad fork **没有**自定义文件加载钩子（`macroquad::file` 只有
///   `set_pc_assets_folder`），拦不住那 77 个读取点（贴图/音频/字体/图标）。
///   所以只能「编进二进制 → 释放到磁盘 → 照常读」。
///
/// 【为什么先往上找一遍】
///   开发时 exe 在 `target/release/`，资源在仓库根 —— 上游的 `init_assets()`
///   本来就是靠「从 exe 往上找含 assets/ 的目录」来定位的。
///   这里**必须先走同一条逻辑**，否则开发时会被判定成「单文件分发」，
///   把 11MB 释放进 `target/release/`，连带把 `data/`（谱面、配置）
///   也搬过去 —— 那是行为变更，不是修 bug。
///
/// 失败不致命：释放不了就原样继续，让 `init_assets()` 抛出那个更明确的
/// `Couldn't load file assets\...` 错误。
fn ensure_embedded_assets() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };

    // ① 沿上游的逻辑往上找 —— 有就直接用，一个字节都不写
    let mut probe = dir.to_path_buf();
    loop {
        if probe.join("assets").join("phigros.ttf").exists() {
            return;
        }
        if !probe.pop() {
            break;
        }
    }

    // ② 往上都没有 → 单文件分发场景，释放到 exe 旁边
    //    （exe 所在目录可能不可写，比如 Program Files → 退到用户临时目录）
    if unpack_assets(&dir.join("assets")).is_err() {
        let fallback = std::env::temp_dir().join("phm-huamei");
        if unpack_assets(&fallback.join("assets")).is_ok() {
            let _ = std::env::set_current_dir(&fallback);
        }
        return;
    }
    // 释放成功 → 交给 init_assets()，它会自己 chdir 到含 assets/ 的那一层
}

fn unpack_assets(root: &std::path::Path) -> std::io::Result<()> {
    for (rel, data) in assets_embedded::ASSETS {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, data)?;
    }
    Ok(())
}

mod dir {
    use anyhow::Result;

    use crate::{CACHE_DIR, DATA_PATH};

    fn ensure(s: &str) -> Result<String> {
        let s = format!("{}/{}", DATA_PATH.lock().unwrap().as_ref().map(|it| it.as_str()).unwrap_or("."), s);
        let path = std::path::Path::new(&s);
        if !path.exists() {
            std::fs::create_dir_all(path)?;
        }
        Ok(s)
    }

    pub fn cache() -> Result<String> {
        if let Some(cache) = &*CACHE_DIR.lock().unwrap() {
            ensure(cache)
        } else {
            ensure("cache")
        }
    }

    pub fn bold_font_path() -> Result<String> {
        Ok(format!("{}/bold.ttf", root()?))
    }

    pub fn cache_image_local() -> Result<String> {
        ensure(&format!("{}/image", cache()?))
    }

    pub fn root() -> Result<String> {
        ensure("data")
    }

    pub fn charts() -> Result<String> {
        ensure("data/charts")
    }

    pub fn collections() -> Result<String> {
        ensure("data/collections")
    }

    pub fn custom_charts() -> Result<String> {
        ensure("data/charts/custom")
    }

    pub fn downloaded_charts() -> Result<String> {
        ensure("data/charts/download")
    }

    pub fn respacks() -> Result<String> {
        ensure("data/respack")
    }
}

async fn the_main() -> Result<()> {
    log::register();
    #[cfg(target_env = "ohos")]
    {
        *DATA_PATH.lock().unwrap() = Some("/data/storage/el2/base".to_owned());
        *CACHE_DIR.lock().unwrap() = Some("/data/storage/el2/base/cache".to_owned());
        prpr::core::DPI_VALUE.store(250, std::sync::atomic::Ordering::Relaxed);
    };

    /* 【话梅 大小核】主线程 = 渲染 + 输入 + 音频回调，必须 P 核。 */
    crate::qos::interactive();

    ensure_embedded_assets();
    init_assets();

    /* 【话梅 大小核】tokio worker 一律降到 UTILITY。
       它们是网络请求、谱面解析、写盘这类「可以慢一点」的活 ——
       默认 QoS 会和渲染线程抢 P 核，那才是真正的掉帧来源。

       ⚠ 注意 `on_thread_start` 只对**新建**线程生效，
       所以必须在 build 之前挂上。 */
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .on_thread_start(|| crate::qos::utility())
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    #[cfg(target_os = "ios")]
    {
        use objc2_foundation::{NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains};

        let directories = NSSearchPathForDirectoriesInDomains(NSSearchPathDirectory::LibraryDirectory, NSSearchPathDomainMask::UserDomainMask, true);
        let path = directories.firstObject().unwrap().to_string();
        *DATA_PATH.lock().unwrap() = Some(path);
        *CACHE_DIR.lock().unwrap() = Some("Caches".to_owned());
    }

    let dir = dir::root()?;
    let data_path = format!("{dir}/data.json");
    /* 【话梅 2026-11-17 存档安全】解析失败**绝不能静默重置**。
       ─────────────────────────────────────────────────────────
       原写法：
           serde_json::from_str(&s).unwrap_or_default()
       问题：data.json 里**任何一个字段**反序列化失败（改了类型、某条记录
       损坏、旧版本字段不兼容），整个 Data 都会变成默认值；而下面第 315 行
       紧接着 `save_data()?` 会立刻把默认值写回原文件 ——
       **玩家的全部存档就永久没了，连恢复的机会都没有。**

       现在的做法：解析失败先把原文件备份成 `data.json.bad-<时间戳>`，
       再退回默认值。这样至少留了证据和可恢复的副本。
       （真正的字段级容错见 Data 上那些 #[serde(default)]；
         这里兜的是它们兜不住的类型错误。） */
    let mut data: Data = match std::fs::read_to_string(&data_path) {
        Ok(s) if !s.trim().is_empty() => match serde_json::from_str::<Data>(&s) {
            Ok(d) => d,
            Err(err) => {
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                let bak = format!("{data_path}.bad-{stamp}");
                if let Err(e) = std::fs::copy(&data_path, &bak) {
                    tracing::error!("data.json 解析失败且备份失败（{e}）：{err}");
                } else {
                    tracing::error!("data.json 解析失败，已备份到 {bak}：{err}");
                }
                Data::default()
            }
        },
        _ => Data::default(),
    };
    data.init().await?;
    set_data(data);
    /* 窗口已经建好（GLKViewController 就在里面），config 也读出来了 ——
       这时才能改渲染帧率。低画质 = 60fps，否则屏幕满帧。 */
    apply_frame_rate(get_data().config.low_quality);
    /* 【话梅 2026-11-17】同步全局 TAA 开关。
       prpr 的 Main 拿不到 phira 的 config，所以用一个原子量解耦。
       之后设置页改 TAA 也要同步更新（见 settings.rs 的 aa_btn 处理）。 */
    prpr::taa::set_global_enabled(get_data().config.taa);
    sync_data();
    save_data()?;

    // Warm up the offline banned-word automaton so local edits can check
    // synchronously. No-op without the `aa` feature.
    tokio::spawn(censor::preload());

    let rx = {
        let (tx, rx) = mpsc::channel();
        *MESSAGES_TX.lock().unwrap() = Some(tx);
        rx
    };

    unsafe { get_internal_gl() }
        .quad_context
        .display_mut()
        .set_pause_resume_listener(on_pause_resume);

    let pgr_font = FontArc::try_from_vec(load_file("phigros.ttf").await?)?;
    PGR_FONT.with(move |it| *it.borrow_mut() = Some(TextPainter::new(pgr_font, None)));

    let font = FontArc::try_from_vec(load_file("font.ttf").await?)?;
    let mut painter = TextPainter::new(font.clone(), None);

    let mut main = Main::new(Box::new(MainScene::new(font).await?), TimeManager::default(), None).await?;

    let tm = TimeManager::default();
    let mut fps_time = -1;

    const FPS_BUF_SIZE: usize = 60;
    let mut fps_times = VecDeque::<f32>::with_capacity(FPS_BUF_SIZE);
    let mut last_frame_start = f32::NAN;
    let mut fps_time_sum = 0.;
    /// perf 心跳计数（每 10 次 = 约 10 秒记一行）
    let mut perf_tick: u32 = 0;

    'app: loop {
        if main.paused() {
            match rx.recv() {
                Ok(false) => {
                    main.resume()?;
                }
                Ok(true) => {}
                Err(_) => break 'app,
            }
        }

        let frame_start = tm.real_time();
        if !last_frame_start.is_nan() {
            if fps_times.len() == FPS_BUF_SIZE {
                fps_time_sum -= fps_times.pop_front().unwrap();
            }
            let frame_time = frame_start as f32 - last_frame_start;
            fps_times.push_back(frame_time);
            fps_time_sum += frame_time;
        }
        last_frame_start = frame_start as f32;
        let res = || -> Result<()> {
            main.update()?;
            main.render(&mut painter)?;
            if let Ok(paused) = rx.try_recv() {
                if paused {
                    main.pause()?;
                } else {
                    main.resume()?;
                }
            }
            prpr::ext::flush_pending_texture_deletions();
            Ok(())
        }();
        if let Err(err) = res {
            error!("uncaught error: {err:?}");
            show_error(err);
        }
        if main.should_exit() {
            break 'app;
        }

        let t = tm.real_time();

        let fps_now = t as i32;
        if fps_now != fps_time {
            fps_time = fps_now;
            if fps_times.len() == FPS_BUF_SIZE {
                let actual_fps = 1. / (fps_time_sum / FPS_BUF_SIZE as f32);
                let current_fps = 1. / (t - frame_start);
                info!("FPS {} (capped at {})", current_fps as u32, actual_fps as u32);

                /* ── 【话梅】帧耗时分位数（2026-11-16，参考 PhiraiAd 的思路）──
                   原来只有 FPS 均值：偶发的 200ms 卡顿会被 59 个好帧稀释，
                   看不出「iOS 耗电」到底卡在哪。P95/P99 和慢帧计数能直接暴露。
                   60 个 f32 排序每秒一次，开销可以忽略；只在诊断通道落盘，
                   正常玩家无感。 */
                let mut sorted: Vec<f32> = fps_times.iter().copied().collect();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let q = |p: usize| sorted[p.min(sorted.len() - 1)] * 1000.;
                let slow = |ms: f32| sorted.iter().filter(|&&x| x * 1000. >= ms).count();
                /* ⚠ 不要每秒都写。
                   原先这段每秒落一次盘 —— iOS 上每次 note() 都是一次文件写入，
                   等于每秒一次 I/O：既费电，又会在 256 KB 轮转里把真正有用的
                   崩溃/诊断信息冲掉（一天 8MB，轮转几十次）。

                   现在只在两种情况下写：
                     · 出现肉眼可见的卡顿（有 >33ms 的帧）→ 立刻记，这是要抓的东西
                     · 否则每 10 秒记一次心跳 → 证明采样还活着
                   正常游玩几乎不产生日志。 */
                let janky = slow(33.33);
                perf_tick += 1;
                if janky > 0 || perf_tick % 10 == 0 {
                    crate::telem::note(format!(
                        "perf: p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms slow(16/33/50/100)={}/{}/{}/{} frames={}",
                        q(FPS_BUF_SIZE * 50 / 100),
                        q(FPS_BUF_SIZE * 95 / 100),
                        q(FPS_BUF_SIZE * 99 / 100),
                        q(FPS_BUF_SIZE - 1),
                        slow(16.67), janky, slow(50.), slow(100.),
                        sorted.len(),
                    ));
                }
            }
        }

        // While backgrounded the scene is paused; the blocking `recv_timeout`
        // above already parks this thread, so nothing extra is needed here.
        next_frame().await;
    }
    Ok(())
}

fn build_global_window_conf() -> Conf {
    let mut conf = build_conf();
    conf.window_title = "Phira".to_owned();
    conf.icon = Some(miniquad::conf::Icon {
        small: *include_bytes!("../icon/small"),
        medium: *include_bytes!("../icon/medium"),
        big: *include_bytes!("../icon/big"),
    });

    #[cfg(target_os = "windows")]
    {
        conf.fullscreen = dir::root()
            .ok()
            .and_then(|r| std::fs::read_to_string(std::path::Path::new(&r).join("data.json")).ok())
            .and_then(|s| serde_json::from_str::<Data>(&s).ok())
            .is_some_and(|d| d.config.fullscreen_mode);
    }

    conf
}

#[no_mangle]
pub extern "C" fn quad_main() {
    /* 设备信息采集放在 Window 之前：这个函数只读系统属性、不碰图形上下文，
       越早越好 —— 一旦开局就要用它填上报载荷了。 */
    crate::device::install();
    macroquad::Window::from_config(build_global_window_conf(), async {
        if let Err(err) = the_main().await {
            error!(?err, "global error");
        }
    });
    cleanup_audio();
}

fn on_pause_resume(pause: bool) {
    /* 【话梅 2026-11-17 修「切后台小概率直接结算」】

       原来这里只往 mpsc 队列塞一条消息，等主循环下一帧处理。
       竞态就在这个「下一帧」：

         锁屏/来电 → applicationWillResignActive → 发消息 → 进程被挂起
                                                  ↑ 如果这一帧还没跑到，
                                                    暂停就永远没生效

       恢复后 `get_time_fn()` 已经前进整段挂起时长，谱面时间直接跳过整首歌，
       音符全 Miss，玩家回来看到的是直接结算。

       **所以这里必须同步冻结时间**，不能依赖主循环。
       `system_pause/resume` 用原子量记录区间，`TimeManager::now()` 会扣掉它。 */
    if pause {
        prpr::time::system_pause();
    } else {
        prpr::time::system_resume();
    }
    // 消息仍然要发 —— 场景层的暂停 UI、音频暂停都靠它
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(pause);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_initializeEnvironment(env: EnvUnowned, _class: JClass) {
    unsafe {
        inputbox::backend::Android::initialize_raw(env.as_raw()).unwrap();
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnPause(_env: EnvUnowned, _class: JClass) {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(true);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnResume(_env: EnvUnowned, _class: JClass) {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(false);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnDestroy(_env: EnvUnowned, _class: JClass) {
    std::process::exit(0);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setDataPath(_env: EnvUnowned, _class: JClass, path: JString) {
    *DATA_PATH.lock().unwrap() = Some(path.to_string());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setTempDir(_env: EnvUnowned, _class: JClass, path: JString) {
    let path = path.to_string();
    std::env::set_var("TMPDIR", path.clone());
    *CACHE_DIR.lock().unwrap() = Some(path);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setDpi(_env: EnvUnowned, _class: JClass, dpi: jint) {
    prpr::core::DPI_VALUE.store(dpi as _, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setChosenFile(_env: EnvUnowned, _class: JClass, file: JString) {
    use prpr::scene::CHOSEN_FILE;
    CHOSEN_FILE.lock().unwrap().1 = Some(file.to_string());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setDeepLink(_env: EnvUnowned, _class: JClass, url: JString) {
    deeplink::set_deeplink(url.to_string());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_markImport(_env: EnvUnowned, _class: JClass) {
    use prpr::scene::CHOSEN_FILE;

    CHOSEN_FILE.lock().unwrap().0 = Some("_import".to_owned());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_markImportRespack(_env: EnvUnowned, _class: JClass) {
    use prpr::scene::CHOSEN_FILE;

    CHOSEN_FILE.lock().unwrap().0 = Some("_import_respack".to_owned());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setInputText(_env: EnvUnowned, _class: JClass, text: JString) {
    use prpr::scene::INPUT_TEXT;
    INPUT_TEXT.lock().unwrap().1 = Some(text.to_string());
}

/// Credentials obtained from the native HYKB (好游快爆) login SDK.
pub struct HykbCredential {
    /// SDK result code: 0 on success, otherwise an error / user cancellation.
    pub code: i32,
    pub uid: i64,
    pub nick: String,
    pub access_token: String,
}

impl HykbCredential {
    /// Map the SDK result code to an error, or yield the credential on success.
    /// Centralizes the code → user-facing message translation shared by every
    /// HYKB login/bind entry point.
    #[cfg(feature = "hykb")]
    pub fn ok_or_err(self) -> Result<Self> {
        if self.code == 0 {
            Ok(self)
        } else {
            // A non-zero code is any failure the HYKB SDK reports: 2001 auth
            // failed, 2002 login failed, 2003 cancelled, 2004 exception, 2005
            // developer-requested exit / account logout. A HYKB build mandates a
            // valid, matching HYKB session, so every one of these must tear the
            // in-game session down — otherwise cancelling the HYKB prompt during
            // a silent re-verify would leave the player signed in and bypass the
            // gate entirely.
            force_logout();
            anyhow::bail!("{}", crate::ttl!("hykb-login-cancelled"))
        }
    }
}

/// Slot for the pending HYKB login result. The native callback fulfills it.
static HYKB_TX: Mutex<Option<tokio::sync::oneshot::Sender<HykbCredential>>> = Mutex::new(None);

/// Call a no-arg `void` method on the Android host activity (the HYKB shell).
#[cfg(all(target_os = "android", feature = "hykb"))]
fn call_activity_void(method: &'static jni::strings::JNIStr) {
    use jni::{jni_sig, objects::JObject, vm::JavaVM};

    JavaVM::singleton()
        .unwrap()
        .attach_current_thread(|env| -> jni::errors::Result<()> {
            let ctx = unsafe { JObject::from_raw(env, ndk_context::android_context().context() as _) };
            env.call_method(ctx, method, jni_sig!("()V"), &[])?;
            Ok(())
        })
        .unwrap();
}

/// Ask the Android shell to pop the HYKB account picker (`MainActivity.hykbSwitchAccount`).
/// Used by the explicit login / switch-account flow.
#[cfg(all(target_os = "android", feature = "hykb"))]
fn request_hykb_login() {
    call_activity_void(jni::jni_str!("hykbSwitchAccount"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
fn request_hykb_login() {}

/// Ask the Android shell to sign in using the cached HYKB account without
/// popping the picker (`MainActivity.hykbLogin`). The credentials the SDK
/// reports flow back through `HYKB_TX`, so the caller can verify them against
/// the restored Phira session. Used by the silent startup restore.
#[cfg(all(target_os = "android", feature = "hykb"))]
fn request_hykb_login_silent() {
    call_activity_void(jni::jni_str!("hykbLogin"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
fn request_hykb_login_silent() {}

/// Tell the native HYKB SDK to sign out (`MainActivity.hykbLogout`). Called when the
/// player logs out from their profile.
#[cfg(all(target_os = "android", feature = "hykb"))]
pub fn hykb_logout() {
    call_activity_void(jni::jni_str!("hykbLogout"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
pub fn hykb_logout() {}

/// Tear down the local session: sign out of the native HYKB SDK, clear the
/// stored account and tokens, then re-sync. Shared by every path that must
/// reject a login — a failed/cancelled HYKB verification, a uid mismatch, or
/// the player logging out from their profile.
pub fn force_logout() {
    hykb_logout();
    get_data_mut().me = None;
    get_data_mut().tokens = None;
    let _ = save_data();
    sync_data();
}

/// Trigger the native HYKB login and await its credentials.
#[allow(unused)]
pub async fn obtain_hykb_credential() -> Result<HykbCredential> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    *HYKB_TX.lock().unwrap() = Some(tx);
    request_hykb_login();
    let cred = rx.await.map_err(|_| anyhow::anyhow!("hykb login cancelled"))?;
    Ok(cred)
}

/// Silently restore the HYKB session from the cached account and await its
/// credentials. Unlike [`obtain_hykb_credential`], this does not pop the account
/// picker; used by the blocking startup check to verify the restored session.
#[allow(unused)]
pub async fn obtain_hykb_credential_silent() -> Result<HykbCredential> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    *HYKB_TX.lock().unwrap() = Some(tx);
    request_hykb_login_silent();
    let cred = rx.await.map_err(|_| anyhow::anyhow!("hykb login cancelled"))?;
    Ok(cred)
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_hykbLoginCallback(
    _env: EnvUnowned,
    _class: JClass,
    code: jint,
    uid: jni::sys::jlong,
    nick: JString,
    access_token: JString,
) {
    let nick = if nick.is_null() { String::new() } else { nick.to_string() };
    let access_token = if access_token.is_null() {
        String::new()
    } else {
        access_token.to_string()
    };
    if let Some(tx) = HYKB_TX.lock().unwrap().take() {
        let _ = tx.send(HykbCredential {
            code: code as i32,
            uid: uid as i64,
            nick,
            access_token,
        });
    } else if code == 2005 {
        // No login is in flight, so this is the SDK's asynchronous
        // anti-addiction "exit game" action: the player hit a play-time limit
        // and chose to quit from the SDK's own dialog. Honor it by exiting.
        // Other async codes (e.g. 2008 "continue playing") are handled inside
        // the SDK and need no response here. A request-less success (code 0, the
        // SDK switching accounts on its own) is likewise ignored: any signed-in
        // HYKB account is accepted, so a switch no longer tears the session down.
    }
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn set_input_text(text: String) {
    use prpr::scene::INPUT_TEXT;
    INPUT_TEXT.lock().unwrap().1 = Some(text);
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn set_chosen_file(file: String) {
    use prpr::scene::CHOSEN_FILE;
    CHOSEN_FILE.lock().unwrap().1 = Some(file);
}

/// 按「低画质模式」设置渲染帧率。
///
/// 120 = 屏幕满帧（ProMotion），60 = 省电档。只改 CADisplayLink 的节奏，
/// 逻辑与判定仍按真实时间推进，不影响手感以外的任何东西。
///
/// 非 iOS 平台是空操作 —— 桌面的帧率由 config.fps_capped 那套老逻辑管。
pub fn apply_frame_rate(low_quality: bool) {
    /* 【话梅】帧率上限改由配置驱动：
         fps_limit = 0   → 不限（iOS 取屏幕满帧 120）
         fps_limit = 60/90/120 → 显式上限
       低画质模式仍然强制 60 —— 它的卖点就是省电，用户既然开了低画质，
       再给 120 是自相矛盾的。 */
    #[cfg(target_os = "ios")]
    {
        /* ⚠ 必须是 crate 内的 get_data()。
           踩过：写成 `prpr::get_data()` —— prpr 里没有这个函数，
           但这段被 `#[cfg(target_os = "ios")]` 包着，**Windows 上不参与编译**，
           所以本地一直绿，直到 iOS 构建才炸。 */
        let limit = crate::get_data().config.fps_limit;
        let fps = if low_quality {
            60
        } else if matches!(limit, 60 | 90 | 120) {
            limit as i32
        } else {
            120
        };
        miniquad::native::ios::set_preferred_fps(fps);
    }
    let _ = low_quality;
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn mark_auto_import() {
    use prpr::scene::CHOSEN_FILE;
    CHOSEN_FILE.lock().unwrap().0 = Some("_import_auto".to_owned());
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn on_foreground() {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(false);
    }
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn on_background() {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(true);
    }
}
