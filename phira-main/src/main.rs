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

fn log_line(text: &str) {
    if let Some(d) = panic_log_dir() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join("panic.log"))
        {
            let _ = f.write_all(text.as_bytes());
            // ⚠ 崩溃路径上进程随时会死，写而不 flush 等于没写
            let _ = f.sync_all();
            let _ = f.flush();
        }
    }
}

fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        log_line(&format!("=== {ts} PANIC ===
{info}

"));
        // 原 hook 照旧（stderr），别把原有行为吃掉
        prev(info);
    }));

    // 装完立刻写一行：区分「没启动」和「启动了没崩」。
    // 上一版没有这行，导致 Documents 为空时无法判断是哪一种。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|x| x.as_secs())
        .unwrap_or(0);
    log_line(&format!(
        "=== {ts} hook installed · dir={} ===
",
        panic_log_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "<none>".into())
    ));
}

/// 把 panic payload 取成人话。
/// `catch_unwind` 拿到的是 `Box<dyn Any>`，常见是 `&str` 或 `String`；
/// 拿不到就退回一句说明——总比空着强。
fn panic_payload_text(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "<非字符串 payload>".to_string()
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
    // ── 关键：拦住 panic，不让它冲过 FFI 边界 ──
    // iOS 上这个 main 是由 ObjC 侧调进来的。Rust 的 panic 一旦越过
    // `extern "C"` 边界，标准库会直接 abort（崩溃报告里就是
    // `panic_cannot_unwind` → `abort`），hook 里的消息虽然写了盘，
    // 但进程当场死，读者只能看到"闪退"。
    // 包一层 catch_unwind 后：panic 被这里接住 → 写清原因 → 正常退出。
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| phira::quad_main()));

    if let Err(e) = result {
        let msg = panic_payload_text(&e);
        log_line(&format!("=== CAUGHT (未 abort) ===
{msg}

"));
        eprintln!("[phira] panic caught: {msg}");
        // 给磁盘一点时间；然后干净退出，别留一个半死的进程
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
