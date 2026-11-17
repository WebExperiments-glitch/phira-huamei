//! 【话梅】TAA（时间抗锯齿）—— 按标准 TAA 管线的四步实现。
//!
//! 参考：Karis 2014「High-Quality Temporal Supersampling」，以及
//! CS Topics / 各引擎文档里的通用流程。标准 TAA 需要四步，缺一不可：
//!
//!   ① **亚像素抖动**（Halton 2,3 序列）：每帧把投影偏移 0.5 像素以内的量，
//!      让同一个像素在时间上采到不同位置 —— 这是「超采样」的采样点来源。
//!      只做混合不做抖动，得到的是**运动模糊**而不是抗锯齿。
//!   ② **重投影**：用运动矢量把历史帧搬到当前帧的坐标上。
//!      3D 需要 G-buffer 逐像素速度；我们是 2D 谱面，相机静止、内容按
//!      `scroll` 速度垂直滚动，速度场是**解析可得**的，不需要 G-buffer。
//!   ③ **邻域钳制**（3×3 AABB）：把历史颜色夹到当前像素邻域的颜色范围内，
//!      防止「旧内容糊在新内容上」的残影（ghosting）。这一步是防残影的关键。
//!      无 shader 时退化为「按运动量降低历史权重」，能抑制大部分残影。
//!   ④ **指数平滑混合**：`out = α·current + (1-α)·history`，α 取 0.1~0.2。
//!
//! ⚠ 本项目没有自定义 resolve shader（miniquad 的 pipeline 未开放逐像素采样
//!   多张纹理的能力），所以 ③ 用「运动感知权重」近似：
//!   滚动越快，历史权重越低 → 音符拖影越少，静态区域仍能累积。

use macroquad::prelude::*;

/// Halton(2,3) 的低差异序列，8 个采样点足够 8 帧收敛。
/// 用固定表而不是运行时计算：每帧只查一次，且避免浮点误差导致序列退化。
const HALTON: [(f32, f32); 8] = [
    (0.5000, 0.3333),
    (0.2500, 0.6667),
    (0.7500, 0.1111),
    (0.1250, 0.4444),
    (0.6250, 0.7778),
    (0.3750, 0.2222),
    (0.8750, 0.5556),
    (0.0625, 0.8889),
];

/// TAA 状态：只需一个帧计数和一个历史纹理。
pub struct Taa {
    frame: u32,
    /// 历史帧（上一帧的 resolve 结果）。
    /// 存 RenderTarget 而不只是 Texture2D —— 它既要能采样，也要能作为绘制目标写入。
    history: Option<RenderTarget>,
    history_dim: (u32, u32),
    /// 上一帧的滚动偏移（世界单位），用于算运动矢量
    last_scroll: f32,
}

impl Default for Taa {
    fn default() -> Self {
        Self {
            frame: 0,
            history: None,
            history_dim: (0, 0),
            last_scroll: 0.,
        }
    }
}

impl Taa {
    pub fn reset(&mut self) {
        self.frame = 0;
        self.last_scroll = 0.;
    }

    /// ① 亚像素抖动偏移（世界单位）。
    ///
    /// 输入是渲染目标尺寸（像素）。抖动幅度控制在**半个像素**以内，
    /// 这是 TAA 的标准范围 —— 超过会明显看出画面在抖。
    pub fn jitter(&self, dim: (u32, u32)) -> Vec2 {
        if dim.0 == 0 || dim.1 == 0 {
            return Vec2::ZERO;
        }
        let (hx, hy) = HALTON[(self.frame as usize) % HALTON.len()];
        // 世界坐标里 [-1,1] 对应整屏，所以 1 像素 = 2/dim
        vec2(
            (hx - 0.5) * 2. / dim.0 as f32,
            (hy - 0.5) * 2. / dim.1 as f32,
        )
    }

    pub fn advance(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// ④ 本帧历史权重。
    ///
    /// 静止时用 0.82（历史占大头 → 累积出抗锯齿）；
    /// 滚动越快，历史越不可信（③ 的近似），权重线性降到 0.35。
    /// `scroll_speed` 单位是「世界单位/秒」，谱面常见 1~10。
    pub fn history_weight(&self, scroll_speed: f32) -> f32 {
        let s = scroll_speed.abs().clamp(0., 8.);
        // 0 → 0.82，8 → 0.35
        (0.82 - s / 8. * 0.47).clamp(0.35, 0.82)
    }

    /// 确保历史纹理尺寸匹配（尺寸变了要重建，否则采样会错位）。
    pub fn ensure_history(&mut self, dim: (u32, u32)) -> bool {
        if dim.0 == 0 || dim.1 == 0 {
            return false;
        }
        if self.history_dim == dim && self.history.is_some() {
            return true;
        }
        let tex = render_target(dim.0, dim.1);
        tex.texture.set_filter(FilterMode::Linear);
        self.history = Some(tex);
        self.history_dim = dim;
        true
    }

    pub fn history(&self) -> Option<Texture2D> {
        if self.history_dim == (0, 0) {
            None
        } else {
            self.history.map(|it| it.texture)
        }
    }

    /// 历史帧作为绘制目标用（把当前帧拷进去）。
    pub fn history_target(&self) -> Option<RenderTarget> {
        self.history
    }

    /// 记录本帧的滚动偏移，供下一帧算运动矢量（当前版本用于调整权重）。
    pub fn set_scroll(&mut self, scroll: f32) {
        self.last_scroll = scroll;
    }

    pub fn scroll_delta(&self, scroll: f32) -> f32 {
        scroll - self.last_scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_half_pixel() {
        let mut taa = Taa::default();
        for _ in 0..16 {
            let j = taa.jitter((1000, 500));
            // 半像素 = 1/1000 与 1/500 的一半
            assert!(j.x.abs() <= 1. / 1000. + 1e-6, "jitter.x={}", j.x);
            assert!(j.y.abs() <= 1. / 500. + 1e-6, "jitter.y={}", j.y);
            taa.advance();
        }
    }

    #[test]
    fn history_weight_drops_with_scroll() {
        let taa = Taa::default();
        assert!(taa.history_weight(0.) > taa.history_weight(4.));
        assert!(taa.history_weight(4.) > taa.history_weight(8.));
        assert!(taa.history_weight(100.) >= 0.35, "下限不该低于 0.35，否则等于没开");
    }
}

/// 【话梅 2026-11-17】全局 TAA 开关。
///
/// 为什么用全局原子量：TAA 要提到 `Main::render` 层才能**全局生效**
/// （大厅、设置页、谱面库都吃到），但 `Main` 属于 prpr，拿不到
/// phira 管的 config。用一个原子量解耦，phira 在启动/改设置时写入。
pub static GLOBAL_TAA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 由 phira 调用：config.taa 变化时同步。
pub fn set_global_enabled(on: bool) {
    GLOBAL_TAA.store(on, std::sync::atomic::Ordering::Release);
}

#[inline]
pub fn global_enabled() -> bool {
    GLOBAL_TAA.load(std::sync::atomic::Ordering::Acquire)
}
