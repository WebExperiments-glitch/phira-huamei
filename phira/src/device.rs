//! 设备与系统信息采集。
//!
//! 【为什么单独一个模块】
//!   iOS 与其它平台的采集方式完全不同，而且 iOS 那段要 objc2 调用 ——
//!   塞进 lib.rs 会把启动流程淹掉。
//!
//! 【原则：采不到就留 None，绝不阻塞】
//!   定位要用户授权、陀螺仪要 CoreMotion。任何一个被拒都不影响开局与上报，
//!   只是那几个字段为 null。用户说「不能就不能吧」，代码要照这个来写。
//!
//! 【这里采什么、不采什么】
//!   采：os_version / model / screen / lat / lon
//!   不采：陀螺仪（要在对局全程采样，属于 game.rs 的事，不在启动时做）
//!
//!   陀螺仪之所以不在这里：它的价值在于「这一局手抖得厉害吗」，
//!   必须跟对局时长对齐采样才有意义。启动时读一次毫无信息量。

use prpr::huamei::DeviceInfo;

/// 采集并写入 prpr 的全局设备信息。**启动时调用一次**。
///
/// 返回值仅用于日志，不影响调用方逻辑。
pub fn install() -> &'static str {
    let mut info = DeviceInfo::default();

    #[cfg(target_os = "ios")]
    {
        unsafe { fill_ios(&mut info) };
    }

    #[cfg(not(target_os = "ios"))]
    {
        /* 非 iOS 平台先只填能跨平台拿到的部分。
           桌面端没有「机型」概念，屏幕尺寸倒是可以拿（macroquad 有），
           但那要等窗口建好 —— 启动早期还拿不到。留空即可，反正这批数据
           现阶段主要看移动端。 */
    }

    /* ⚠ 必须打日志。2026-11-15 第一版 device.rs 上线后，服务端收到的
       device 全是 null —— 但「采集失败」和「采集成功但字段没填」在
       上报载荷里长得一模一样（都是 {}），完全没法区分。
       这里把三件事都记下来：拿到几个字段、具体值、是否写入全局。 */
    let has = info.os_version.is_some() || info.model.is_some() || info.screen.is_some();
    tracing::info!(
        "device: os={:?} model={:?} screen={:?} -> {}",
        info.os_version,
        info.model,
        info.screen,
        if has { "collected" } else { "empty" }
    );
    prpr::huamei::set_device_info(info);
    if has { "collected" } else { "empty" }
}

#[cfg(target_os = "ios")]
unsafe fn fill_ios(info: &mut DeviceInfo) {
    use objc2::MainThreadMarker;
    use objc2_ui_kit::{UIScreen, UIDevice};

    /* ── 系统版本 ──
       UIDevice.systemVersion 给的是 "17.0" 这种。
       ⚠ 不要用 operatingSystemVersionString —— 那串在部分系统上是
         "Version 17.0 (Build 21A329)" 这种英文长句，长短不一且不可解析。 */
    /* ⚠ objc2-ui-kit 0.3 起，所有 UI 相关的构造都要求 MainThreadMarker ——
       它在类型层面保证这些调用发生在主线程（UIKit 不是线程安全的）。
       device::install() 由 quad_main 调用，本来就在主线程，所以这里必然成功。
       拿不到只能是调用方写错了，此时直接放弃采集，不影响游戏启动。 */
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("device: 拿不到 MainThreadMarker，跳过采集");
        return;
    };

    let dev = UIDevice::currentDevice(mtm);
    let ver = dev.systemVersion();
    let ver_str = ver.to_string();
    if !ver_str.is_empty() {
        info.os_version = Some(ver_str);
    }

    /* ── 机型标识 ──
       UIDevice.model 只给 "iPhone" / "iPad"，没有具体型号。
       真正的硬件标识（"iPhone15,2"）要问 sysctl hw.machine —— 这是
       Apple 自己也在用的办法，不涉及私有 API。 */
    if let Some(m) = machine_model() {
        info.model = Some(m);
    }

    /* ── 屏幕 ──
       bounds 是**逻辑点**，scale 是像素比。
       两者一起才能还原真实分辨率：logical × scale = physical。 */
    let screen = UIScreen::mainScreen(mtm);
    let bounds = screen.bounds();
    let scale = screen.scale() as f32;
    let w = bounds.size.width as f32;
    let h = bounds.size.height as f32;
    if w > 0.0 && h > 0.0 && scale > 0.0 {
        info.screen = Some([w, h, scale]);
    }
}

/// 读 sysctl `hw.machine` —— 得到 "iPhone15,2" 这类硬件标识。
///
/// 用 sysctl 而不是 UIDevice.model：后者只返回产品线名（"iPhone"），
/// 无法区分具体机型，对「机型性能 vs 判定偏差」的分析没有价值。
#[cfg(target_os = "ios")]
unsafe fn machine_model() -> Option<String> {
    let mut size: libc::size_t = 0;
    let name = b"hw.machine\0";
    if libc::sysctlbyname(name.as_ptr() as *const libc::c_char, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 {
        return None;
    }
    if size == 0 || size > 256 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    if libc::sysctlbyname(
        name.as_ptr() as *const libc::c_char,
        buf.as_mut_ptr() as *mut libc::c_void,
        &mut size,
        std::ptr::null_mut(),
        0,
    ) != 0
    {
        return None;
    }
    // sysctl 返回的是 NUL 结尾的 C 字符串，size 含结尾的 0
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}
