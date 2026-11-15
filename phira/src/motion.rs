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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// 全局采样器。对局开始时 reset，结算时 take。
static SAMPLER: Mutex<Sampler> = Mutex::new(Sampler::new());
static AVAILABLE: AtomicBool = AtomicBool::new(false);

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

/// 启动陀螺仪。幂等 —— 重复调用不会创建第二个 manager。
pub fn start() {
    #[cfg(target_os = "ios")]
    {
        if !AVAILABLE.load(Ordering::Relaxed) {
            let ok = unsafe { imp::start_updates() };
            AVAILABLE.store(ok, Ordering::Relaxed);
            if !ok {
                tracing::warn!("motion: deviceMotion 启动失败（无陀螺仪或权限），陀螺仪数据将为空");
            }
        }
    }
}

/// 对局开始时调用：清空上一局的样本。
pub fn begin_round() {
    if let Ok(mut s) = SAMPLER.lock() {
        s.reset();
    }
}

/// 每帧调用一次。内部会判断是否可以采（未 begin / 不可用都会直接返回）。
pub fn sample_frame() {
    if !AVAILABLE.load(Ordering::Relaxed) {
        return;
    }
    #[cfg(target_os = "ios")]
    {
        let Some(m) = (unsafe { imp::read_magnitude() }) else {
            return;
        };
        if let Ok(mut s) = SAMPLER.lock() {
            s.push(m);
        }
    }
}

/// 结算时调用：取出摘要并停止累积。
pub fn finish_round() -> Option<[f32; 3]> {
    SAMPLER.lock().ok().and_then(|mut s| s.summarize())
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
            mgr.setDeviceMotionUpdateInterval(1.0 / 60.0);
            mgr.startDeviceMotionUpdates();
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
