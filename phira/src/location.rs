//! 定位：只取**城市级**经纬度，用于「不同地区玩家的设备与网络状况」这一维。
//!
//! 【为什么只到城市级】
//!   1. 我们不需要更细 —— 分析维度是「南方/北方」「一线/非一线」，不是「哪个小区」
//!   2. 更细的坐标是敏感信息，存进库里就是负债
//!   所以拿到之后立刻四舍五入到 2 位小数（≈1km），原始值不进内存之外的地方。
//!
//! 【为什么用轮询而不是 delegate】
//!   CLLocationManager 是 delegate 模式 + 异步授权，Rust 侧要实现
//!   `CLLocationManagerDelegate` 的 trait 对象、处理 block 生命周期、
//!   保证回调线程安全 —— 复杂度远超「读一个坐标」这件事。
//!
//!   游戏本来每帧都跑，开头几秒顺手读 `location` 即可：
//!   授权弹窗出来时读不到（None），用户点了允许、系统定上位之后就有了。
//!
//! 【拒绝授权】
//!   恒为 None。不报错、不重试、不弹二次提示。用户的选择就是最终结果。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// 已拿到的城市级坐标 [lat, lon]。None = 还没定上位或被拒。
static COORD: Mutex<Option<[f64; 2]>> = Mutex::new(None);
/// 是否已经发起过授权请求（避免每帧重复调）
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// 定位管理器是否可用（模拟器上没有）
static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// 启动定位。幂等。**会触发系统授权弹窗**（第一次调用时）。
pub fn start() {
    if REQUESTED.swap(true, Ordering::Relaxed) {
        return;
    }

    #[cfg(target_os = "ios")]
    {
        let ok = unsafe { imp::start_updates() };
        AVAILABLE.store(ok, Ordering::Relaxed);
        if !ok {
            tracing::warn!("location: CLLocationManager 启动失败，定位数据将为空");
        }
    }
}

/// 每帧调用一次，尝试把已拿到的坐标收进来。
///
/// ⚠ 只在还没拿到时尝试 —— 拿到一次就够了，后续不再覆盖
/// （玩家可能在打歌途中移动，但我们要的是「在哪儿打的」，取第一次最接近）。
pub fn sample_frame() {
    if !AVAILABLE.load(Ordering::Relaxed) {
        return;
    }
    if COORD.lock().map(|c| c.is_some()).unwrap_or(true) {
        return;
    }
    #[cfg(target_os = "ios")]
    {
        if let Some(c) = unsafe { imp::read_coord() } {
            tracing::info!("location: 已定位到 {:.2},{:.2}（城市级）", c[0], c[1]);
            if let Ok(mut slot) = COORD.lock() {
                *slot = Some(c);
            }
        }
    }
}

/// 取当前坐标。没有就 None。
pub fn get() -> Option<[f64; 2]> {
    COORD.lock().ok().and_then(|c| *c)
}

/// 给上报用：现场采一次再返回。
///
/// ⚠ 为什么不做成「每帧采」：定位不像陀螺仪那样需要连续采样 ——
///   我们只要「在哪儿打的」，一个点就够。每帧读没有额外信息，
///   还会让 location 一直更新（玩家换姿势/移动都会触发）。
///   所以只在结算构造载荷时采一次。
pub fn coord_for_upload() -> Option<[f64; 2]> {
    sample_frame();
    get()
}

#[cfg(target_os = "ios")]
mod imp {
    use objc2::rc::Retained;
    use objc2_core_location::CLLocationManager;
    use std::cell::RefCell;

    thread_local! {
        /* 必须保持存活 —— drop 掉 manager 就停止更新了。 */
        static MANAGER: RefCell<Option<Retained<CLLocationManager>>> = const { RefCell::new(None) };
    }

    pub unsafe fn start_updates() -> bool {
        MANAGER.with(|cell| {
            if cell.borrow().is_some() {
                return true;
            }
            let mgr = CLLocationManager::new();
            /* 精度只要到城市级 —— 用 NearestTenMeters 反而更费电、更敏感，
               而且我们最后还会四舍五入到 1km。 */
            mgr.setDesiredAccuracy(1000.0);
            /* 触发授权弹窗。用户拒绝后这个调用不会再有反应，
               location 恒为 None —— 这是预期行为。 */
            mgr.requestWhenInUseAuthorization();
            mgr.startUpdatingLocation();
            *cell.borrow_mut() = Some(mgr);
            true
        })
    }

    pub unsafe fn read_coord() -> Option<[f64; 2]> {
        MANAGER.with(|cell| {
            let borrowed = cell.borrow();
            let mgr = borrowed.as_ref()?;
            let loc = mgr.location()?;
            let c = loc.coordinate();
            /* 降到城市级：2 位小数 ≈ 1.1km（纬度）。
               ⚠ 这一步必须在**写入全局之前**做 —— 原始坐标不该活过这个函数。 */
            let lat = (c.latitude * 100.0).round() / 100.0;
            let lon = (c.longitude * 100.0).round() / 100.0;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return None;
            }
            Some([lat, lon])
        })
    }
}
