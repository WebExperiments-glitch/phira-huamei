//! Time manager for music time and real time synchronization.

use crate::config::Config;

pub struct TimeManager {
    pub adjust_time: bool,
    pub start_time: f64,
    pause_time: Option<f64>,
    pub speed: f64,
    pub force: f64,
    wait: f64,

    /// 本实例创建/reset 时的「全局累计暂停」快照。
    ///
    /// ⚠ 必须按实例做差，不能用全局绝对值：
    /// `reset()` 会把 `start_time` 拉到当前时刻，如果那时仍减去全局累计的
    /// 10 秒，`now()` 会直接变成 **负数**（时间倒退，判定全乱）。
    /// 存快照后只减「本实例生命周期内新增的暂停」。
    sys_pause_base: f64,

    get_time_fn: Box<dyn Fn() -> f64>,
}

impl Default for TimeManager {
    fn default() -> Self {
        Self::new(1.0, false)
    }
}


// ── 系统级暂停（切后台/锁屏）────────────────────────────────
//
// 【为什么不能只靠 mpsc 消息】
//
// UI 线程在 `applicationWillResignActive` 里只往队列塞一条消息
// （phira/src/lib.rs 的 on_pause_resume），真正的暂停要等主循环下一帧。
// 如果进程在那之前被挂起 —— 加载中、大帧、锁竞争 —— 暂停就永远不生效。
//
// 恢复后 `get_time_fn()` 已经前进了整段挂起时长，`now()` 直接跳过几十秒：
// 整首歌的音符全部 Miss，玩家回来看到的是**直接结算**。
// 这就是「切后台小概率直接结算」的机制，本质是竞态。
//
// 所以这里在 UI 线程**同步**记录暂停区间，`now()` 再把它扣掉，
// 完全不依赖主循环是否来得及跑。

use std::sync::{
    atomic::{AtomicU64, Ordering},
    LazyLock,
};

/// 全局单调时钟原点。所有测量都相对它，保证不同 TimeManager 实例
/// （各自有不同的 `start_time`）能共用同一份暂停累计。
static EPOCH: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);

#[inline]
fn mono_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

/// 当前暂停区间的起点（毫秒）。`u64::MAX` 表示「没在暂停」。
static PAUSE_BEGIN_MS: AtomicU64 = AtomicU64::new(u64::MAX);
/// 累计已暂停的毫秒数。
static PAUSE_TOTAL_MS: AtomicU64 = AtomicU64::new(0);

/// 系统进入后台/锁屏。**在 UI 线程同步调用**，可重入（重复调用无副作用）。
pub fn system_pause() {
    // compare_exchange 保证只有第一次生效，嵌套调用不会覆盖起点
    let _ = PAUSE_BEGIN_MS.compare_exchange(
        u64::MAX,
        mono_ms(),
        Ordering::AcqRel,
        Ordering::Relaxed,
    );
}

/// 系统回到前台。把这段暂停时长累加进总数。
pub fn system_resume() {
    let begin = PAUSE_BEGIN_MS.swap(u64::MAX, Ordering::AcqRel);
    if begin != u64::MAX {
        let now = mono_ms();
        // 防御：时钟异常导致 now < begin 时不计（宁可不补，也不要倒扣）
        if now > begin {
            PAUSE_TOTAL_MS.fetch_add(now - begin, Ordering::AcqRel);
        }
    }
}

/// 累计暂停秒数。`now()` 会把它减掉。
#[inline]
pub fn system_paused_secs() -> f64 {
    PAUSE_TOTAL_MS.load(Ordering::Relaxed) as f64 / 1000.
}

impl TimeManager {
    pub fn from_config(config: &Config) -> Self {
        Self::new(1., config.adjust_time)
    }

    pub fn manual(get_time_fn: Box<dyn Fn() -> f64>) -> Self {
        let start_time = get_time_fn();
        Self {
            adjust_time: false,
            start_time,
            pause_time: None,
            speed: 1.0,
            wait: f64::NEG_INFINITY,
            force: 3e-3,
            sys_pause_base: system_paused_secs(),

            get_time_fn,
        }
    }

    pub fn new(speed: f64, adjust_time: bool) -> Self {
        // we use performance.now() on web since audioContext.currentTime is not stable
        // and may cause serious latency problem
        #[cfg(target_arch = "wasm32")]
        let get_time_fn = {
            let perf = web_sys::window().unwrap().performance().unwrap();
            move || perf.now() / 1000.
        };
        #[cfg(not(target_arch = "wasm32"))]
        let get_time_fn = {
            let start = std::time::Instant::now();
            move || start.elapsed().as_secs_f64()
        };
        let t = get_time_fn();
        Self {
            adjust_time,
            start_time: t,
            pause_time: None,
            speed,
            wait: f64::NEG_INFINITY,
            force: 3e-3,
            sys_pause_base: system_paused_secs(),

            get_time_fn: Box::new(get_time_fn),
        }
    }

    pub fn real_time(&self) -> f64 {
        (self.get_time_fn)()
    }

    pub fn reset(&mut self) {
        self.start_time = self.real_time();
        self.pause_time = None;
        self.wait = f64::NEG_INFINITY;
        // 基准同步前移：之后的 now() 只扣「reset 之后新增」的暂停
        self.sys_pause_base = system_paused_secs();
    }

    pub fn wait(&mut self) {
        self.wait = self.real_time() + 0.1;
    }

    pub fn dont_wait(&mut self) {
        self.wait = f64::NEG_INFINITY;
    }

    #[must_use]
    pub fn now(&self) -> f64 {
        /* 减去系统级暂停（切后台/锁屏）。
           这一步是「小概率直接结算」的修复核心：即使 mpsc 消息没来得及被主循环处理，
           挂起的那段时间也不会算进谱面时间。 */
        (self.pause_time.unwrap_or_else(&self.get_time_fn)
            - self.start_time
            - (system_paused_secs() - self.sys_pause_base))
            * self.speed
    }

    pub fn update(&mut self, music_time: f64) {
        if !self.adjust_time || self.real_time() <= self.wait || self.pause_time.is_some() {
            return;
        }

        /* ── 自动对齐：必须限幅，否则一次异常能把时钟推反 ──
         *
         * 【为什么】
         *   `start_time -= (music_time - now()) * force` 本身没问题 ——
         *   force 只有 3e-3，每帧只挪一点点，看起来温和。
         *
         *   但 music_time 来自音频后端（`self.music.position()`）。打歌中
         *   退出再回来、按下音量键、音频会话被系统中断 —— 这些情况下
         *   position() 可能返回 0、负数、或者跳回几秒前。
         *
         *   一旦 music_time 突然变负，diff 变成大负数，
         *   start_time 被推**正向**，于是 now() 开始递减：
         *     · 倒计时不再走 3,2,1，而是往上数 3,4,5…
         *     · 谱面时间倒退，音符倒着飞（2026-11-15 实机复现）
         *
         * 【修法】分三档：
         *   · |diff| < 0.08   忽略（正常抖动，不值得追）
         *   · |diff| < 0.5    温和追（原来的行为）
         *   · |diff| >= 0.5   认为音频时间不可信，**不动时钟**
         *                     —— 宁可音画不同步，也不能让时间倒流。
         *                        不同步是可以听出来的、可恢复的；时间倒流
         *                        会让整个判定系统进入不可恢复的错乱。
         *
         *   ⚠ music_time 非有限值（NaN / inf）同样跳过 —— 那说明音频后端
         *     已经给出垃圾数据，此刻任何数值运算都会污染 start_time。 */
        if !music_time.is_finite() {
            tracing::warn!("time: music_time 非有限值（{}），跳过自动对齐", music_time);
            return;
        }
        let diff = music_time - self.now();
        if !diff.is_finite() || diff.abs() < 0.08 {
            return;
        }
        if diff.abs() >= 0.5 {
            tracing::warn!(
                "time: 音频时间偏差 {}s 超过阈值，跳过自动对齐（不追，避免时间倒流）", diff
            );
            return;
        }
        self.start_time -= diff * self.force;
    }

    #[must_use]
    pub fn paused(&self) -> bool {
        self.pause_time.is_some()
    }

    pub fn pause(&mut self) {
        self.pause_time = Some(self.real_time());
    }

    pub fn resume(&mut self) {
        /* ⚠ 没有 pause 过就不能 resume —— 这是合法状态，不是错误。
           原来写的是 `self.pause_time.take().unwrap()`，在 iOS 上必然炸：
           应用冷启动 / 从后台恢复时，系统直接进入 resume 路径，而此前
           并没有走过 pause()，于是 pause_time 是 None。

           后果极其隐蔽（2026-11-15 实机 375 条日志才挖出来）：
             · prpr/src/time.rs:106 一次 panic
             · 主 async 块（phira/src/lib.rs:386）future 中毒
             · 之后每帧 `async fn resumed after panicking`，374 次
             · 画面抽搐、黑块，但进程不死 —— 看着像显卡问题，其实是这里

           pause_time 为空就什么都不做：时间基准本来就没被 pause 改过。 */
        if let Some(pause_time) = self.pause_time.take() {
            self.start_time += self.real_time() - pause_time;
        }
        self.wait();
    }

    pub fn seek_to(&mut self, pos: f64) {
        self.start_time = self.pause_time.unwrap_or_else(&self.get_time_fn) - pos / self.speed;
        self.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* 这几个测试都操作**全局**原子量（PAUSE_TOTAL_MS / PAUSE_BEGIN_MS），
       而 cargo test 默认多线程跑 —— 不加锁会互相污染，
       表现为「补偿只生效了一部分」的诡异误差。 */
    static TEST_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    /// 系统暂停后，now() 不应该因为挂起期间的真实时间流逝而前进。
    #[test]
    fn system_pause_freezes_now() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut tm = TimeManager::new(1., false);
        // 模拟已经跑了 0.2 秒
        tm.start_time -= 0.2;
        let before = tm.now();

        system_pause();
        std::thread::sleep(std::time::Duration::from_millis(120));
        system_resume();

        let after = tm.now();
        // 挂起 120ms，但 now() 只该前进接近 0（误差容忍 50ms 的调度抖动）
        assert!(
            (after - before).abs() < 0.05,
            "挂起期间时间不该前进：before={before} after={after}"
        );
    }

    /// reset 之后，历史累计的暂停不能被重复扣除（否则 now() 变负数）。
    #[test]
    fn reset_does_not_double_count_history() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        system_pause();
        std::thread::sleep(std::time::Duration::from_millis(60));
        system_resume();

        let mut tm = TimeManager::new(1., false);
        tm.reset();
        let v = tm.now();
        assert!(v >= 0., "reset 后 now() 必须是非负，实际 {v}");
    }

    /// 嵌套 pause 只记第一次的起点。
    #[test]
    fn nested_pause_is_idempotent() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tm = TimeManager::new(1., false);
        let before = tm.now();
        system_pause();
        system_pause();
        std::thread::sleep(std::time::Duration::from_millis(50));
        system_resume();
        let after = tm.now();
        assert!((after - before).abs() < 0.05, "嵌套暂停不该叠加计费");
    }
}
