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
/// 依次试可写目录：HOME/Documents → TMPDIR → 当前目录。
/// iOS 侧载下 HOME 可能为空（2026-11-06 实测 Documents 全空），不能只依赖它。
fn panic_log_dir() -> Option<std::path::PathBuf> {
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            cands.push(std::path::PathBuf::from(h).join("Documents"));
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            cands.push(std::path::PathBuf::from(h).join("Library").join("Caches"));
        }
    }
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            cands.push(std::path::PathBuf::from(t));
        }
    }
    cands.push(std::path::PathBuf::from("."));
    for d in cands {
        if std::fs::create_dir_all(&d).is_ok() {
            if std::fs::OpenOptions::new().create(true).append(true).open(d.join("panic.log")).is_ok() {
                return Some(d);
            }
        }
    }
    None
}

fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = panic_log_dir();
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
                // ⚠ 崩溃发生在 GLKView 的 C 回调里，panic 后进程立刻 abort，
                // 没有机会 flush；必须显式同步到磁盘。
                let _ = f.sync_all();
                let _ = f.flush();
            }
        }
        // 原 hook 照旧（stderr），别把原有行为吃掉
        prev(info);
    }));

    // 装完立刻写一行：区分「没启动」和「启动了没崩」。
    // 上一版没有这行，导致 Documents 为空时无法判断是哪一种。
    if let Some(d) = panic_log_dir() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("panic.log")) {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "=== {ts} hook installed · dir={} ===", d.display());
            let _ = f.sync_all();
        }
    }
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
