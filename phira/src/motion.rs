//! 陀螺仪采样：对局期间按帧累积，结算时算摘要。
//!
//! 【为什么不传完整采样】
//!   一局 3 分钟 @60Hz = 10,800 个三元组，约 130 KB JSON。
//!   而且没人会去分析 1 万个点 —— 真正有信息量的是「这一局手稳不稳」，
//!   三个数就够了：[均值, 标准差, 峰值]（rad/s）。
//!
//! 【为什么是轮询而不是 startGyroUpdatesToQueue】
//!   CoreMotion 的 block 回调要跨 FFI 传闭包，Rust 侧要构造 block 指针
//!   并保证生命周期，还涉及多线程 —— 复杂度远超收益。
//!   游戏本来每帧都在跑，顺手读一次 `deviceMotion` 即可。
//!
//! 【拒绝授权 / 设备没有陀螺仪】
//!   `startDeviceMotionUpdates` 失败时 `deviceMotion()` 永远返回 None，
//!   采样器保持空状态，摘要为 None。**不报错、不阻塞**。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use crate::telem::note;
#[cfg(target_os = "ios")]
use crate::telem::note_warn;

/// 全局采样器。对局开始时 reset，结算时 take。
static SAMPLER: Mutex<Sampler> = Mutex::new(Sampler::new());
/// startDeviceMotionUpdates 是否已成功启动。
static AVAILABLE: AtomicBool = AtomicBool::new(false);
/// 是否已经**尝试过**启动。只试一次 —— 失败就是失败，每帧重试只会
/// 每帧新建一个 CMMotionManager 并把日志刷满。
static START_TRIED: AtomicBool = AtomicBool::new(false);

/* ── 诊断计数 ────────────────────────────────────────────────
   gyro 曾经 100% 是 null 而且零报错，因为三种失败长得一样：
   ① 没启动 ② 启动了但 deviceMotion() 恒 None ③ 采到了但没累积。
   这三个计数器把它们区分开，一局只打 3~4 行日志。 */
static FRAMES: AtomicU32 = AtomicU32::new(0);
static NO_DATA_FRAMES: AtomicU32 = AtomicU32::new(0);
static HAD_SAMPLE: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Sampler {
    /// 只累积模长，不存三轴 —— 省内存，且「手抖幅度」用一个数就够了
    magnitudes: Vec<f32>,
    /// 峰值（模长的最大值）
    peak: f32,
    running: bool,
}

impl Sampler {
    const fn new() -> Self {
        Self {
            magnitudes: Vec::new(),
            peak: 0.0,
            running: false,
        }
    }

    fn reset(&mut self) {
        self.magnitudes.clear();
        self.peak = 0.0;
        self.running = true;
    }

    fn push(&mut self, m: f32) {
        if !self.running || !m.is_finite() {
            return;
        }
        /* 手机上晃一下通常 0.5~15 rad/s。
           > 50 基本是传感器噪声或瞬间冲击（比如摔手机），留着会污染均值。 */
        if m > 50.0 {
            return;
        }
        if m > self.peak {
            self.peak = m;
        }
        self.magnitudes.push(m);
    }

    /// 算出 [均值, 标准差, 峰值]；样本太少（< 10 帧）视为没采到。
    fn summarize(&mut self) -> Option<[f32; 3]> {
        self.running = false;
        let n = self.magnitudes.len();
        if n < 10 {
            return None;
        }
        let mean = self.magnitudes.iter().sum::<f32>() / n as f32;
        let var = self.magnitudes.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n as f32;
        Some([mean, var.sqrt(), self.peak])
    }
}

/// 【已废弃 2026-11-15】不要在启动时调 —— 保留函数只为兼容旧调用点。
///
/// ⚠ 真机实测：在 quad_main（Window 创建之前）调 startDeviceMotionUpdates
///   会**静默失效** —— 那时 run loop 还没跑起来，CoreMotion 的更新不会发生，
///   deviceMotion() 恒为 None。表现就是 gyro 一直是 null。
///
///   现在改成**懒启动**：第一次 sample_frame 时才启动，那时游戏循环已经在跑。
pub fn start() {
    /* 什么都不做，真正的启动在 sample_frame 里。 */
}

/// 对局开始时调用（含重开）：清空上一局的样本。
pub fn begin_round() {
    FRAMES.store(0, Ordering::Relaxed);
    NO_DATA_FRAMES.store(0, Ordering::Relaxed);
    HAD_SAMPLE.store(false, Ordering::Relaxed);
    let ok = SAMPLER.lock().map(|mut s| s.reset()).is_ok();
    note(format!("begin_round，采样器已清零（running=true: {ok}）"));
}

/// 每帧调用一次。内部会判断是否可以采（未 begin / 不可用都会直接返回）。
pub fn sample_frame() {
    /* 懒启动：第一次采样时才开始更新。
       此时游戏循环已经在跑，run loop 是活的 —— 这是 CoreMotion 能工作的前提。
       在 Window 创建之前调 startDeviceMotionUpdates 会静默失效（实测）。 */
    if !START_TRIED.load(Ordering::Relaxed) {
        START_TRIED.store(true, Ordering::Relaxed);
        #[cfg(target_os = "ios")]
        {
            let ok = unsafe { imp::start_updates() };
            AVAILABLE.store(ok, Ordering::Relaxed);
            if ok {
                note("deviceMotion 已启动（懒启动，首次采样时）");
            } else {
                note_warn("deviceMotion 启动失败（isDeviceMotionAvailable=false）—— 本局不会有 gyro");
            }
        }
        #[cfg(not(target_os = "ios"))]
        AVAILABLE.store(false, Ordering::Relaxed);
        return;
    }

    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;

    #[cfg(target_os = "ios")]
    {
        if !AVAILABLE.load(Ordering::Relaxed) {
            return;
        }
        match unsafe { imp::read_magnitude() } {
            Some(m) => {
                if !HAD_SAMPLE.swap(true, Ordering::Relaxed) {
                    note(format!("第 {frame} 帧拿到首个样本 {m:.3} rad/s"));
                }
                if let Ok(mut s) = SAMPLER.lock() {
                    s.push(m);
                }
            }
            None => {
                let n = NO_DATA_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
                /* 第 1 帧 None 是正常的（更新还没来），第 60 帧还没有就要怀疑
                   run loop 模式 / 权限了。三行之后不再打，避免刷屏。 */
                if n == 1 || n == 60 || n == 600 {
                    note_warn(format!("第 {frame} 帧 deviceMotion 仍为 None（累计 {n} 帧无数据）"));
                }
            }
        }
    }
    #[cfg(not(target_os = "ios"))]
    let _ = frame;
}

/// 结算时调用：取出摘要并停止累积。
pub fn finish_round() -> Option<[f32; 3]> {
    let frames = FRAMES.load(Ordering::Relaxed);
    let no_data = NO_DATA_FRAMES.load(Ordering::Relaxed);
    let mut samples = 0usize;
    let mut running = false;
    let mut out = None;
    if let Ok(mut s) = SAMPLER.lock() {
        samples = s.magnitudes.len();
        running = s.running;
        out = s.summarize();
    }
    /* 这一行是判定 gyro 为什么是 null 的唯一凭据：
       running=false → begin_round 没被调用（本次修的 bug）
       running=true, samples=0 → deviceMotion 一直 None（看上面的 warn）
       samples>0 但 out=None → 样本不足 10 帧（局太短） */
    note(format!(
        "finish_round frames={frames} no_data={no_data} running={running} samples={samples} -> {out:?}"
    ));
    out
}

#[cfg(target_os = "ios")]
mod imp {
    use objc2::rc::Retained;
    use objc2_core_motion::CMMotionManager;
    use std::cell::RefCell;

    thread_local! {
        /* 复用同一个 manager。
           ⚠ 必须保持存活 —— 一旦 drop，startDeviceMotionUpdates 的
             更新就停了，之后 deviceMotion() 恒为 None。 */
        static MANAGER: RefCell<Option<Retained<CMMotionManager>>> = const { RefCell::new(None) };
    }

    pub unsafe fn start_updates() -> bool {
        MANAGER.with(|cell| {
            if cell.borrow().is_some() {
                return true;
            }
            let mgr = CMMotionManager::new();
            /* 60Hz 够用：游戏本身 60fps，采更密只是多占内存。
               ⚠ deviceMotion 要求设备有陀螺仪；模拟器上 available 为 false。 */
            if !mgr.isDeviceMotionAvailable() {
                return false;
            }
            super::note("isDeviceMotionAvailable=true，开始 startDeviceMotionUpdates");
            mgr.setDeviceMotionUpdateInterval(1.0 / 60.0);
            mgr.startDeviceMotionUpdates();
            /* 立刻回读一次 active：如果这里是 false，说明 start 调用没被接受
               （模拟器 / 无陀螺仪 / 权限），后面 deviceMotion() 必然是 None。
               这条日志把「调用失败」和「调用成功但没数据」分开。 */
            super::note(format!(
                "start 之后 active={} interval={:.4}s",
                mgr.isDeviceMotionActive(),
                mgr.deviceMotionUpdateInterval()
            ));
            *cell.borrow_mut() = Some(mgr);
            true
        })
    }

    pub unsafe fn read_magnitude() -> Option<f32> {
        MANAGER.with(|cell| {
            let borrowed = cell.borrow();
            let mgr = borrowed.as_ref()?;
            let data = mgr.deviceMotion()?;
            let r = data.rotationRate();
            /* rotationRate 是三轴角速度（rad/s）。取模长 = 「转得多快」，
               与方向无关 —— 玩家怎么拿手机都行，我们只关心稳不稳。 */
            let (x, y, z) = (r.x as f32, r.y as f32, r.z as f32);
            Some((x * x + y * y + z * z).sqrt())
        })
    }
}
