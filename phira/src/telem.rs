//! 诊断日志：**同时**写 NSLog 和 App 沙盒里的 `Documents/motion.log`。
//!
//! 【为什么需要这个模块】
//!   `tracing` 最终走 miniquad 的 `log!`，iOS 上就是 NSLog —— 要连 Console.app
//!   或 idevicesyslog 才看得到。而开发机是 Windows、没有 Mac，于是「日志打出来了」
//!   和「日志看不到」没有区别。`gyro` 恒为 null 的问题因此拖了两版才定位。
//!
//!   Info.plist 开了 `UIFileSharingEnabled`，Documents 里的文件可以在
//!   「文件」App 里直接拿到（我的 iPad → phira-hm → motion.log），
//!   这是当前唯一不依赖 Mac 的读取通道。
//!
//! 【为什么叫 motion.log 而不是 telem.log】
//!   这个文件名已经在真机上验证过、也写在交接文档里了，改名只会让下一个人
//!   两头找。内容以后可以不止陀螺仪。

use std::path::PathBuf;

/// 日志目录 —— **全 crate 唯一**的一份路径策略。
///
/// 之前这里和 `phira-main/src/main.rs` 的 `panic_log_dir()` 各写了一份，
/// 一处改了另一处不会跟着改，日志就会分裂到两个目录里。
/// 现在两边都走这个函数。
///
/// 顺序：HOME/Documents（用户在「文件」App 里看得见）→ HOME/Library/Caches
/// → TMPDIR → 都没有就 None（调用方自己决定退化成只打 NSLog）。 */
pub fn log_dir() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            cands.push(PathBuf::from(&h).join("Documents"));
            cands.push(PathBuf::from(&h).join("Library").join("Caches"));
        }
    }
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            cands.push(PathBuf::from(t));
        }
    }
    cands.into_iter().find(|d| std::fs::create_dir_all(d).is_ok())
}

/// 一条普通诊断。iOS 上同时落盘。
pub fn note(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    tracing::info!("motion: {msg}");
    #[cfg(target_os = "ios")]
    sink::append(msg);
}

/// `note` 的**单态**版本。
/// 泛型函数 `note::<T>` 不能直接当 `fn(&str)` 用（每个 T 是一个不同的函数项），
/// 而 `PlatformHooks.log_line` 要的是具体函数指针。
pub fn note_str(s: &str) {
    note(s);
}

/// 一条告警级诊断。iOS 上同时落盘，并带 `WARN` 前缀。
#[cfg(target_os = "ios")]
pub fn note_warn(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    tracing::warn!("motion: {msg}");
    sink::append(&format!("WARN {msg}"));
}

#[cfg(target_os = "ios")]
mod sink {
    use std::io::Write;

    /// 单文件上限 256 KB，超了就轮转一份。沙盒空间有限，真要看也只看最近几局。
    const MAX_BYTES: u64 = 256 * 1024;

    pub fn append(line: &str) {
        let Some(d) = super::log_dir() else { return };
        let path = d.join("motion.log");
        if let Ok(m) = std::fs::metadata(&path) {
            if m.len() > MAX_BYTES {
                let _ = std::fs::rename(&path, d.join("motion.log.1"));
            }
        }
        let ts = chrono::Local::now().format("%m-%d %H:%M:%S%.3f");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "[{ts}] {line}");
        }
    }
}
