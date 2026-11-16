//! 【话梅】线程 QoS 分级 —— 让大核跑关键线程，小核跑杂活。
//!
//! ## 原理
//!
//! Apple Silicon 是大小核（P 核 / E 核）。**没有**公开的核心绑核 API，
//! 系统靠 **QoS 类**（Quality of Service）推断该把线程放哪：
//!
//! | QoS 类 | 值 | 典型落点 |
//! |---|---|---|
//! | `USER_INTERACTIVE` | 0x21 | P 核（主线程 / 渲染 / 音频） |
//! | `USER_INITIATED` | 0x19 | P 核 |
//! | `DEFAULT` | 0x15 | 两者皆可 |
//! | `UTILITY` | 0x11 | 多为 E 核（I/O、下载、解析） |
//! | `BACKGROUND` | 0x09 | E 核，且**可能长时间不被调度** |
//!
//! ## ⚠ 必须说清的局限（这不是银弹）
//!
//! Radar FB24980619（2026-09-28，iOS 27，至今 Open）记录了 Flutter 的实测：
//! 渲染线程明明设了 `USER_INTERACTIVE`，**仍然几乎全被调度到 E 核并掉帧**，
//! 因为该线程每帧只忙 11%。给线程加上人为负载后，系统才把它挪回 P 核。
//!
//! **所以 QoS 是提示（hint），不是保证。** 系统同时看「实际负载占比」，
//! 线程太闲就会被降级，QoS 再高也压不住。
//!
//! 我们能做的上限是：
//! 1. 关键线程声明最高 QoS
//! 2. **把后台任务压到低 QoS**，减少它们和关键线程抢 E 核
//! 3. 不要让后台任务以 DEFAULT/高 QoS 跑（那是把大核让给杂活）
//!
//! 剩下的取决于系统调度，实机上只能靠**耗电与掉帧率**间接验证。
//!
//! ## 平台差异
//!
//! 这些函数是 Apple 专有的（`_np` = nonportable）。Windows/Android 没有对应概念，
//! 所以全部 `#[cfg(target_os = "ios")]`，其他平台是空操作 —— 这样调用点不用到处加 cfg。

/// QoS 类取值（来自 `<pthread/qos.h>`）。
pub mod class {
    pub const USER_INTERACTIVE: u32 = 0x21;
    pub const USER_INITIATED: u32 = 0x19;
    pub const DEFAULT: u32 = 0x15;
    pub const UTILITY: u32 = 0x11;
    pub const BACKGROUND: u32 = 0x09;
}

/// 给**当前**线程设 QoS。非 Apple 平台是空操作。
///
/// `relative_priority` 允许在同一类内微调（0 表示类默认），
/// 这里统一用 0 —— 我们不需要那么细的控制。
pub fn set_current(qos_class: u32) {
    #[cfg(target_os = "ios")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // 返回值失败也不处理：QoS 只是提示，设不上不影响正确性。
        let _ = unsafe { pthread_set_qos_class_self_np(qos_class, 0) };
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = qos_class;
    }
}

/// 主/渲染线程：必须 P 核。
pub fn interactive() {
    set_current(class::USER_INTERACTIVE);
}

/// 后台工作线程（网络、解析、写盘）：交给 E 核。
pub fn utility() {
    set_current(class::UTILITY);
}

/// 纯算力且用户不直接等待的任务。
///
/// ⚠ 慎用：Apple 文档明确警告，`BACKGROUND` 的线程**可能很久完全不被调度**。
/// 用户正在等结果的任务（如 PoW 解算）不要用这一级。
pub fn background() {
    set_current(class::BACKGROUND);
}
