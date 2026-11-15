//! Time manager for music time and real time synchronization.

use crate::config::Config;

pub struct TimeManager {
    pub adjust_time: bool,
    pub start_time: f64,
    pause_time: Option<f64>,
    pub speed: f64,
    pub force: f64,
    wait: f64,

    get_time_fn: Box<dyn Fn() -> f64>,
}

impl Default for TimeManager {
    fn default() -> Self {
        Self::new(1.0, false)
    }
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
    }

    pub fn wait(&mut self) {
        self.wait = self.real_time() + 0.1;
    }

    pub fn dont_wait(&mut self) {
        self.wait = f64::NEG_INFINITY;
    }

    #[must_use]
    pub fn now(&self) -> f64 {
        (self.pause_time.unwrap_or_else(&self.get_time_fn) - self.start_time) * self.speed
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
