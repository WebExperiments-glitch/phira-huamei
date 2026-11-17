//! 【话梅】主页装扮素材导入：从用户选中的文件（桌面文件对话框 / iOS 文件 App）
//! 复制进资源包 `home/` 目录，并自动设置 `config.home_bg` / `home_bgm`。
//!
//! ## 为什么不直接引用原始路径
//!
//! iOS 的照片库 / 文件 App 给的是**安全作用域 URL**，出了那次授权就失效。
//! 所以必须把文件**复制进我们自己的沙盒**（`data/respack/home/`），
//! 之后主页加载走的就是稳定的本地路径。
//!
//! ## 权限
//!
//! · iOS 文件 App（UIDocumentPicker）不需要额外权限 —— 用户选什么给什么
//! · iOS 相册需要 `NSPhotoLibraryUsageDescription`（见 phira.app/Info.plist）
//! · 相机（拍照）暂未实现，不申请 NSCameraUsageDescription

use std::path::Path;

/// 素材类型，决定写入哪个配置项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoKind {
    Background,
    Music,
}

impl DecoKind {
    /// 按扩展名判断用户选的文件属于哪一类。
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => Some(Self::Background),
            "ogg" | "mp3" | "wav" | "m4a" | "flac" => Some(Self::Music),
            _ => None,
        }
    }

    fn subdir(self) -> &'static str {
        match self {
            Self::Background => "images",
            Self::Music => "audio",
        }
    }
}

/// 把选中的文件复制进资源包，返回相对路径（供 config 使用）。
///
/// 文件名加时间戳前缀，避免「换了张图却因为同名被覆盖、缓存不刷新」。
pub fn import_file(src: &Path, kind: DecoKind) -> anyhow::Result<String> {
    let root = crate::dir::respacks()?;
    let dir = Path::new(&root).join("home").join(kind.subdir());
    std::fs::create_dir_all(&dir)?;

    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%f");
    let name = format!("{stamp}.{ext}");
    let dst = dir.join(&name);
    std::fs::copy(src, &dst)?;

    Ok(format!("home/{}/{}", kind.subdir(), name))
}

/// 导入后写配置并存档。
pub fn apply_import(rel: String, kind: DecoKind) -> anyhow::Result<()> {
    {
        let data = crate::get_data_mut();
        match kind {
            DecoKind::Background => data.config.home_bg = Some(rel),
            DecoKind::Music => {
                data.config.home_bgm = Some(rel);
                // 触发主页 BGM 热重载
                crate::scene::BGM_RELOAD.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
    crate::save_data()
}

/// 桌面端：弹出系统文件对话框，选完直接导入。
///
/// iOS / Android 走各自的原生选择器（见 `picker_ios`），
/// 因为 `rfd` 在移动端的支持不完整。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn pick_and_import(kind: DecoKind) -> Option<anyhow::Result<String>> {
    let mut dialog = rfd::FileDialog::new().set_title(match kind {
        DecoKind::Background => "选择主页背景图片",
        DecoKind::Music => "选择主页背景音乐",
    });
    dialog = dialog.add_filter(
        match kind {
            DecoKind::Background => "图片",
            DecoKind::Music => "音频",
        },
        match kind {
            DecoKind::Background => &["png", "jpg", "jpeg", "webp", "gif", "bmp"],
            DecoKind::Music => &["ogg", "mp3", "wav", "m4a", "flac"],
        },
    );
    let path = dialog.pick_file()?;
    Some(import_file(&path, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_extension() {
        assert_eq!(DecoKind::from_path(Path::new("a/foo.PNG")), Some(DecoKind::Background));
        assert_eq!(DecoKind::from_path(Path::new("a/foo.webp")), Some(DecoKind::Background));
        assert_eq!(DecoKind::from_path(Path::new("a/foo.mp3")), Some(DecoKind::Music));
        assert_eq!(DecoKind::from_path(Path::new("a/foo.FLAC")), Some(DecoKind::Music));
        assert_eq!(DecoKind::from_path(Path::new("a/foo.txt")), None);
    }
}


// ── iOS：文件 App / 相册选择器 ──────────────────────────────
//
// 复用 library.rs 里现成的 UIDocumentPickerViewController 模式。
// 关键点：选中的 URL 是**安全作用域**的，必须 startAccessingSecurityScopedResource
// 才能读；读完复制到我们自己的沙盒后再 stop，之后走稳定本地路径。

#[cfg(target_os = "ios")]
mod picker_ios {
    use super::*;
    use objc2::{define_class, rc::Retained, runtime::ProtocolObject, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSURL};
    use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController};
    use std::cell::RefCell;

    thread_local! {
        static DELEGATE: RefCell<Option<Retained<DecoPickerDelegate>>> = const { RefCell::new(None) };
        /// 记录本次选择是给背景还是音乐用的（delegate 回调里需要知道）
        static PENDING_KIND: std::cell::Cell<Option<DecoKind>> = const { std::cell::Cell::new(None) };
    }

    define_class! {
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        struct DecoPickerDelegate;

        unsafe impl NSObjectProtocol for DecoPickerDelegate {}

        unsafe impl UIDocumentPickerDelegate for DecoPickerDelegate {
            #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
            fn did_pick_documents_at_urls(
                &self,
                _controller: &UIDocumentPickerViewController,
                urls: &NSArray<NSURL>,
            ) {
                let kind = PENDING_KIND.with(|k| k.take());
                let Some(kind) = kind else { return };
                let Some(url) = urls.firstObject() else { return };

                // 安全作用域：不 start 就读不到文件
                let accessing = unsafe { url.startAccessingSecurityScopedResource() };
                let path = url.path().to_string();
                let result = import_file(Path::new(&path), kind)
                    .and_then(|rel| apply_import(rel, kind));
                if accessing {
                    unsafe { url.stopAccessingSecurityScopedResource() };
                }
                match result {
                    Ok(()) => tracing::info!("home_picker: 导入成功"),
                    Err(err) => tracing::warn!("home_picker: 导入失败 {err}"),
                }
            }

            #[unsafe(method(documentPickerWasCancelled:))]
            fn document_picker_was_cancelled(&self, _controller: &UIDocumentPickerViewController) {
                PENDING_KIND.with(|k| k.set(None));
            }
        }
    }

    impl DecoPickerDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { objc2::msg_send![super(this), init] }
        }
    }

    pub fn pick_and_import(kind: DecoKind) {
        let mtm = MainThreadMarker::new().expect("home_picker: 必须在主线程调用");
        PENDING_KIND.with(|k| k.set(Some(kind)));

        // UTType：图片用 public.image，音频用 public.audio
        // 用 NSString 构造，避免依赖 UniformTypeIdentifiers 的具体版本 API
        let type_id = match kind {
            DecoKind::Background => "public.image",
            DecoKind::Music => "public.audio",
        };
        let types = NSArray::from_retained_slice(&[objc2_foundation::NSString::from_str(type_id)]);

        let picker = UIDocumentPickerViewController::alloc(mtm);
        /* 用 `initWithDocumentTypes:inMode:` 而不是 14.0 的
           `initForOpeningContentTypes:asCopy:` —— 后者要 NSArray<UTType>，
           而我们传的是 UTI 字符串（NSString）。老 API 从 iOS 8 就有，
           虽然标记 deprecated，但行为一致且类型匹配。
           （export 那边的模板也是用 UTI 字符串走老 API。） */
        #[allow(deprecated)]
        let picker = {
            use objc2_ui_kit::UIDocumentPickerMode;
            UIDocumentPickerViewController::initWithDocumentTypes_inMode(
                picker,
                &types,
                UIDocumentPickerMode::Import,
            )
        };

        let delegate = DecoPickerDelegate::new(mtm);
        picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        DELEGATE.with(|it| *it.borrow_mut() = Some(delegate));

        if let Some(controller) = inputbox::backend::IOS::get_top_view_controller(mtm) {
            controller.presentViewController_animated_completion(&picker, true, None);
        } else {
            tracing::warn!("home_picker: 找不到顶层视图控制器，无法弹出文件选择器");
        }
    }
}

#[cfg(target_os = "ios")]
pub use picker_ios::pick_and_import;
