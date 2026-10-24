/// 【为什么要有这个】—— 给 iOS 用
///
/// 2026-10-24 实测：iOS 上 Rust panic 只会 SIGABRT，`.ips` 崩溃报告里
/// 只有一句 `abort() called`，既没有 panic 消息，帧也没符号（二进制被剥离）。
/// **光看崩溃报告完全查不出原因。**
///
/// 所以在这里装一个 panic hook，把消息 + 位置追加写进 App 沙盒的
/// `Documents/panic.log`。配合 Info.plist 的 `UIFileSharingEnabled`，
/// 用户可以直接在「文件」App 里打开看到，不需要连电脑。
///
/// ⚠ iOS 上 `HOME` 环境变量指向 App 容器（不是 /var/mobile），
///   所以 `$HOME/Documents` 就是可写且可被用户访问的位置。
///   桌面端走同一条路径也行（HOME 是真实家目录），日志落在 ~/Documents。
fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = if home.is_empty() {
            None
        } else {
            let d = std::path::PathBuf::from(&home).join("Documents");
            std::fs::create_dir_all(&d).ok();
            Some(d)
        };
        let text = format!(
            "=== {} ===
{info}

",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        if let Some(d) = dir {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(d.join("panic.log"))
            {
                let _ = f.write_all(text.as_bytes());
                let _ = f.flush();
            }
        }
        // 原 hook 照旧（stderr），别把原有行为吃掉
        prev(info);
    }));
}

fn main() {
    install_panic_log();
    // Deeplink support
    for arg in std::env::args().skip(1) {
        let arg = arg.trim().to_owned();
        if arg.contains("://") {
            phira::deeplink::set_deeplink(arg);
            break;
        }
    }
    phira::quad_main();
}
