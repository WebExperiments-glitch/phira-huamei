//! Scene management module.
#![allow(unused_macros)]

prpr_l10n::tl_file!("scene" ttl);

mod ending;
pub use ending::{EndingScene, RecordUpdateState};

mod game;
pub use game::{GameMode, GameScene, SimpleRecord};

mod loading;
pub use loading::{BasicPlayer, LoadingScene, SaveFn, UpdateFn, UploadFn};

use crate::{
    ext::{draw_image, screen_aspect, LocalTask, SafeTexture, ScaleType},
    judge::Judge,
    time::TimeManager,
    ui::{BillBoard, Dialog, Message, MessageHandle, MessageKind, TextPainter, Ui},
};
use anyhow::{Error, Result};
use cfg_if::cfg_if;
use inputbox::{
    backend::{default_backend, Backend},
    InputBox,
};
use macroquad::prelude::*;
use std::{
    any::Any,
    borrow::Cow,
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tracing::warn;

#[derive(Default)]
pub enum NextScene {
    #[default]
    None,
    Pop,
    PopN(usize),
    PopWithResult(Box<dyn Any>),
    PopNWithResult(usize, Box<dyn Any>),
    Exit,
    Overlay(Box<dyn Scene>),
    Replace(Box<dyn Scene>),
}

thread_local! {
    pub static BILLBOARD: RefCell<(BillBoard, TimeManager)> = RefCell::new((BillBoard::new(), TimeManager::default()));
    pub static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
    pub static FULL_LOADING: RefCell<Option<FullLoadingView>> = const { RefCell::new(None) };
}

pub struct FullLoadingView {
    keep_alive: Arc<()>,
    text: Option<Cow<'static, str>>,
}

impl FullLoadingView {
    pub fn begin() -> Arc<()> {
        Self::begin_inner(None)
    }
    pub fn begin_text(text: Cow<'static, str>) -> Arc<()> {
        Self::begin_inner(Some(text))
    }
    fn begin_inner(text: Option<Cow<'static, str>>) -> Arc<()> {
        let arc = Arc::new(());
        let ret = arc.clone();
        FULL_LOADING.replace(Some(Self { keep_alive: arc, text }));
        ret
    }
}

#[inline]
pub fn show_error(error: Error) {
    warn!("show error: {error:?}");
    Dialog::error(error).show();
}

pub struct MessageBuilder {
    content: String,
    kind: MessageKind,
    duration: f32,
}

impl MessageBuilder {
    pub fn new(content: String) -> Self {
        Self {
            content,
            kind: MessageKind::Info,
            duration: 2.,
        }
    }

    #[inline]
    pub fn kind(mut self, kind: MessageKind) -> Self {
        self.kind = kind;
        self
    }

    #[inline]
    pub fn duration(mut self, t: f32) -> Self {
        self.duration = t;
        self
    }

    #[inline]
    pub fn ok(self) -> Self {
        self.kind(MessageKind::Ok)
    }

    #[inline]
    pub fn warn(self) -> Self {
        self.kind(MessageKind::Warn)
    }

    #[inline]
    pub fn error(self) -> Self {
        self.kind(MessageKind::Error)
    }

    fn show(&mut self) -> MessageHandle {
        BILLBOARD.with(|it| {
            let mut guard = it.borrow_mut();
            let (msg, handle) = Message::new(std::mem::take(&mut self.content), guard.1.now() as _, self.duration, self.kind.clone());
            guard.0.add(msg);
            handle
        })
    }

    #[inline]
    pub fn handle(mut self) -> MessageHandle {
        let handle = self.show();
        std::mem::forget(self);
        handle
    }
}

impl Drop for MessageBuilder {
    fn drop(&mut self) {
        self.show();
    }
}

#[inline]
pub fn show_message(msg: impl Into<String>) -> MessageBuilder {
    MessageBuilder::new(msg.into())
}

pub static INPUT_TEXT: Mutex<(Option<String>, Option<String>)> = Mutex::new((None, None));
/// Holds the id of the last input request the user cancelled (clicked Cancel or
/// dismissed the dialog). Consumed via [`take_input_cancelled`]; distinct from
/// [`INPUT_TEXT`] so callers can distinguish "cancelled" from "no input yet".
pub static INPUT_CANCELLED: Mutex<Option<String>> = Mutex::new(None);
#[cfg(not(target_arch = "wasm32"))]
pub static CHOSEN_FILE: Mutex<(Option<String>, Option<String>)> = Mutex::new((None, None));

fn show_inputbox(config: InputBox, backend: &dyn Backend) {
    let result = config.show_with_async(backend, |result| match result {
        Ok(Some(text)) => {
            INPUT_TEXT.lock().unwrap().1 = Some(text);
        }
        Ok(None) => {
            // User cancelled; report it under the pending request's id so the
            // caller can react (e.g. return to the previous screen).
            let id = INPUT_TEXT.lock().unwrap().0.clone();
            *INPUT_CANCELLED.lock().unwrap() = id;
        }
        Err(err) => {
            warn!(?err, "failed to get input");
        }
    });
    if let Err(err) = result {
        warn!(?err, "failed to show input box");
    }
}

/// 【huamei 改动】最近一次 `request_input` 是不是**多行**模式。
///
/// 为什么要记这个：输入框回传的文本在 Windows 上会被**子进程自己的噪声**污染
/// （见 `sanitize_dialog_text`）。单行模式下真值一定是**最后一行**，
/// 所以可以放心只取最后一行；多行模式下用户的内容本身就有换行，
/// 那样切会把用户输入切掉一大半 —— 必须区别对待。
static INPUT_MULTILINE: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn request_input(id: impl Into<String>, mut config: InputBox) {
    INPUT_MULTILINE.store(config.mode == inputbox::InputMode::Multiline, Ordering::Relaxed);
    *INPUT_TEXT.lock().unwrap() = (Some(id.into()), None);
    *INPUT_CANCELLED.lock().unwrap() = None;
    if config.title.is_none() {
        config = config.title(ttl!("input"));
    }
    if config.prompt.is_none() {
        config = config.prompt(ttl!("input-msg"));
    }
    if config.cancel_label.is_none() {
        config = config.cancel_label(ttl!("cancel"));
    }
    if config.ok_label.is_none() {
        config = config.ok_label(ttl!("confirm"));
    }
    show_inputbox(config, &*default_backend());
}

pub fn take_input() -> Option<(String, String)> {
    let multiline = INPUT_MULTILINE.load(Ordering::Relaxed);
    let mut w = INPUT_TEXT.lock().unwrap();
    w.0.clone()
        .zip(std::mem::take(&mut w.1).map(|it| sanitize_dialog_text(&it, multiline)))
}

/// 【huamei 改动】修输入框在 Windows 上回传文本的**两层损坏**。
///
/// ══════════════════════════════════════════════════════════════
/// 【第一层：UTF-16 被当成 UTF-8 读】（2026-10-11 实测）
/// ══════════════════════════════════════════════════════════════
/// Windows 上输入框是 `inputbox` crate 跑系统脚本宿主 + WinForms 弹原生窗，
/// 而 Rust 侧取的是 **`output.stdout` 的全部内容**当返回值：
/// ```ignore
/// Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
/// ```
/// 实测宿主**按 UTF-16 LE（带 BOM）写出**，原始字节是：
/// ```text
/// ff fe | 70 00 79 00 72 00 | 0d 00 0a 00
///  BOM  |  p \0  y \0  r \0 |  \r \0 \n \0
/// ```
/// 按 UTF-8 读就变成 `\u{FFFD}\u{FFFD}p\0y\0r\0…`
/// —— **每个字符后面多一个 NUL，长度接近翻倍**，屏幕上完全看不出来。
///
/// ══════════════════════════════════════════════════════════════
/// 【第二层：子进程自己的告警粘在文本前面】
/// ══════════════════════════════════════════════════════════════
/// 实测 `data/huamei-login.log` 抓到：
/// ```text
/// raw = "[WARNING] Ignore caches that are heterogeneous\r\npyzpansiyuan123456@qq.com"
/// ```
/// 子进程在初始化 WinForms/GDI+ 时把自己显卡/字体缓存的告警也写进了 stdout。
/// 18 个字符的邮箱加上这行 48 字符的告警，就是用户看到的「59 个字符」。
///
/// 【修法】
///   1. 删 `\0` / `U+FFFD`（第一层）
///   2. **单行模式**下只取**最后一行**（第二层）—— 脚本是最后才写文本框内容的，
///      真值在末尾；噪声都是整行的，按换行切就够。
///      ⚠ 多行模式**不能**切，用户的内容本身就有换行。
///
/// ⚠ 空白**不在这里动** —— 搜索词、谱名里的空格是合法的。
///   要删空白的场合自己去做（邮箱 → `phira::huamei::normalize_email`）。
pub fn sanitize_dialog_text(s: &str, multiline: bool) -> String {
    let cleaned: String = if s.contains(['\0', '\u{FFFD}']) {
        s.chars().filter(|c| !matches!(*c, '\0' | '\u{FFFD}')).collect()
    } else {
        s.to_owned()
    };
    if multiline {
        return cleaned.trim_end_matches(['\r', '\n']).to_owned();
    }
    // 单行：取最后一段非空内容。`str::lines()` 会把 `\r\n` 的 `\r` 也吃掉。
    cleaned
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// Returns the id of a cancelled input request once, clearing it.
pub fn take_input_cancelled() -> Option<String> {
    INPUT_CANCELLED.lock().unwrap().take()
}

pub fn return_input(id: String, text: String) {
    *INPUT_TEXT.lock().unwrap() = (Some(id), Some(text));
}

#[cfg(not(target_arch = "wasm32"))]
pub fn request_file(id: impl Into<String>) {
    let id: String = id.into();
    #[cfg(target_env = "ohos")]
    let is_photo = id == "avatar";
    *CHOSEN_FILE.lock().unwrap() = (Some(id), None);
    cfg_if! {
        if #[cfg(target_os = "android")] {
            unsafe {
                let env = miniquad::native::attach_jni_env();
                let ctx = ndk_context::android_context().context();
                let class = (**env).GetObjectClass.unwrap()(env, ctx);
                let method = (**env).GetMethodID.unwrap()(env, class, c"chooseFile".as_ptr() as _, c"()V".as_ptr() as _);
                (**env).CallVoidMethod.unwrap()(env, ctx, method);
            }
        } else if #[cfg(target_os = "ios")] {
            use objc2::{available, define_class, rc::Retained, runtime::ProtocolObject, MainThreadMarker, MainThreadOnly};
            use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
            use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController};

            thread_local! {
                static DELEGATE: RefCell<Option<Retained<PickerDelegate>>> = const { RefCell::new(None) };
            }

            define_class! {
                // SAFETY:
                // - The superclass NSObject does not have any subclassing requirements.
                // - `Delegate` does not implement `Drop`.
                #[unsafe(super = NSObject)]
                #[thread_kind = MainThreadOnly]
                struct PickerDelegate;

                // SAFETY: `NSObjectProtocol` has no safety requirements.
                unsafe impl NSObjectProtocol for PickerDelegate {}

                // SAFETY: `UIDocumentPickerDelegate` has no safety requirements.
                unsafe impl UIDocumentPickerDelegate for PickerDelegate {
                    // SAFETY: The signature is correct.
                    #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
                    fn did_pick_documents_at_urls(&self, controller: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
                        use objc2_foundation::{NSData, NSDataReadingOptions, NSTemporaryDirectory};

                        let url = urls.firstObject().unwrap();
                        let need_close = unsafe { url.startAccessingSecurityScopedResource() };

                        let data = match NSData::dataWithContentsOfURL_options_error(&url, NSDataReadingOptions::Uncached) {
                            Ok(data) => data,
                            Err(err) => {
                                let message = err.localizedDescription().to_string();
                                show_error(Error::msg(message).context(ttl!("read-file-failed")));
                                return;
                            }
                        };
                        if need_close {
                            unsafe { url.stopAccessingSecurityScopedResource() };
                        }

                        let dir = NSTemporaryDirectory();
                        let path = format!("{}{}", dir, uuid::Uuid::new_v4());
                        data.writeToFile_atomically(&NSString::from_str(&path), true);
                        CHOSEN_FILE.lock().unwrap().1 = Some(path);
                    }
                }
            }

            impl PickerDelegate {
                fn new(mtm: MainThreadMarker) -> Retained<Self> {
                    let this = Self::alloc(mtm).set_ivars(());
                    unsafe { objc2::msg_send![super(this), init] }
                }
            }

            let mtm = MainThreadMarker::new().unwrap();

            let picker = UIDocumentPickerViewController::alloc(mtm);
            let picker = if available!(ios = 14.0.0) {
                use objc2_uniform_type_identifiers::UTType;

                /* ── 用 public.item，而不是按扩展名逐个枚举（2026-11-16 修白屏）──
                 *
                 * 原来传的是 [zip, pez, jpg, png, jpeg, json, mp3, ogg] 各自的
                 * UTType。问题在于 `typeWithFilenameExtension` 对 **未注册的扩展**
                 * 会返回一个动态类型（dyn.*），而文件浏览器的行为是**只显示
                 * 与这些类型一致的条目** —— pez 这类冷门扩展一旦退化成动态类型，
                 * 浏览器就可能一条都匹配不上，表现为「标题和侧栏都在、内容区全白」。
                 *
                 * 换成 public.item（所有文件的根类型）后浏览器不再做过滤，
                 * 用户能看到并选择任意文件；选错的文件由导入逻辑按扩展名拒绝，
                 * 失败路径本来就有。 */
                let item = UTType::typeWithIdentifier(&NSString::from_str("public.item"))
                    .expect("public.item 是文件系统的根类型，系统必然提供");
                let types = NSArray::from_retained_slice(&[item]);
                UIDocumentPickerViewController::initForOpeningContentTypes(picker, &types)
            } else {
                #[allow(deprecated)]
                {
                    use objc2_ui_kit::UIDocumentPickerMode;

                    let ext = NSString::from_str;
                    let types = NSArray::from_retained_slice(&[ext("public.image"), ext("public.archive")]);
                    UIDocumentPickerViewController::initWithDocumentTypes_inMode(picker, &types, UIDocumentPickerMode::Import)
                }
            };
            let dlg_obj = PickerDelegate::new(mtm);
            picker.setDelegate(Some(ProtocolObject::from_ref(&*dlg_obj)));
            DELEGATE.with(|it| *it.borrow_mut() = Some(dlg_obj));

            /* 呈现者必须是**我们窗口里的根控制器**。
               `connectedScenes` 在本项目不可靠（legacy 窗口，没有 scene manifest），
               从错误的 VC 呈现会得到「弹窗在、内容空白」的文件选择器。 */
            let presented = match miniquad::native::ios::root_view_controller_raw() {
                Some(raw) => {
                    use objc2_ui_kit::UIViewController;
                    let vc: &UIViewController = unsafe { &*(raw as *const UIViewController) };
                    vc.presentViewController_animated_completion(&picker, true, None);
                    true
                }
                None => false,
            };
            if !presented {
                match inputbox::backend::IOS::get_top_view_controller(mtm) {
                    Some(vc) => {
                        vc.presentViewController_animated_completion(&picker, true, None);
                    }
                    None => {
                        show_error(Error::msg("no view controller to present the file picker"));
                        return;
                    }
                }
            }
            tracing::info!("picker: 已呈现（root_vc={presented}）");
        } else if #[cfg(target_env = "ohos")] {
            miniquad::native::call_request_callback(format!(r#"{{"action": "chooseFile", "isPhoto": {}}}"#, is_photo));
        } else { // desktop
            CHOSEN_FILE.lock().unwrap().1 = rfd::FileDialog::new().pick_file().map(|it| it.display().to_string());
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn take_file() -> Option<(String, String)> {
    let mut w = CHOSEN_FILE.lock().unwrap();
    w.0.clone().zip(std::mem::take(&mut w.1))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn return_file(id: String, file: String) {
    *CHOSEN_FILE.lock().unwrap() = (Some(id), Some(file));
}

pub trait Scene {
    fn enter(&mut self, _tm: &mut TimeManager, _target: Option<RenderTarget>) -> Result<()> {
        Ok(())
    }
    fn pause(&mut self, _tm: &mut TimeManager) -> Result<()> {
        Ok(())
    }
    fn resume(&mut self, _tm: &mut TimeManager) -> Result<()> {
        Ok(())
    }
    fn on_result(&mut self, _tm: &mut TimeManager, _result: Box<dyn Any>) -> Result<()> {
        Ok(())
    }
    fn touch(&mut self, _tm: &mut TimeManager, _touch: &Touch) -> Result<bool> {
        Ok(false)
    }
    fn update(&mut self, tm: &mut TimeManager) -> Result<()>;
    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()>;
    /// 该场景是否**自己**处理抗锯齿（有自己的离屏目标 + 时间累积）。
    ///
    /// 为 true 时，`Main::render` 的全局 TAA 会让位 —— 否则对局画面会被
    /// 混合两次，结果是明显发糊而不是更干净。
    fn handles_own_aa(&self) -> bool {
        false
    }

    fn next_scene(&mut self, _tm: &mut TimeManager) -> NextScene {
        NextScene::None
    }
}

pub trait RenderTargetChooser {
    fn choose(&mut self) -> Option<RenderTarget>;
}
impl RenderTargetChooser for Option<RenderTarget> {
    fn choose(&mut self) -> Option<RenderTarget> {
        *self
    }
}
impl<F: FnMut() -> Option<RenderTarget>> RenderTargetChooser for F {
    fn choose(&mut self) -> Option<RenderTarget> {
        self()
    }
}

pub struct Main {
    pub scenes: Vec<Box<dyn Scene>>,
    times: Vec<f64>,
    target_chooser: Box<dyn RenderTargetChooser>,
    tm: TimeManager,
    paused: bool,
    last_update_time: f64,
    should_exit: bool,
    pub top_level: bool,
    touches: Option<Vec<Touch>>,
    pub viewport: Option<(i32, i32, i32, i32)>,

    /// 【话梅 2026-11-17】全局 TAA 状态（大厅/设置页也生效）。
    taa: crate::taa::Taa,
    /// 全局 TAA 的离屏目标（尺寸 = 窗口）
    taa_target: Option<RenderTarget>,
    taa_dim: (u32, u32),
}

impl Main {
    pub async fn new(mut scene: Box<dyn Scene>, mut tm: TimeManager, mut target_chooser: impl RenderTargetChooser + 'static) -> Result<Self> {
        simulate_mouse_with_touch(false);
        scene.enter(&mut tm, target_chooser.choose())?;
        let last_update_time = tm.now();
        macro_rules! load_tex {
            ($path:literal) => {
                SafeTexture::from(Texture2D::from_image(&load_image($path).await?))
            };
        }
        let icons = [load_tex!("info.png"), load_tex!("warn.png"), load_tex!("ok.png"), load_tex!("error.png")];
        BILLBOARD.with(|it| it.borrow_mut().0.set_icons(icons));
        Ok(Self {
            scenes: vec![scene],
            times: Vec::new(),
            target_chooser: Box::new(target_chooser),
            tm,
            paused: false,
            last_update_time,
            should_exit: false,
            top_level: true,
            touches: None,
            viewport: None,
            taa: Default::default(),
            taa_target: None,
            taa_dim: (0, 0),
        })
    }

    pub fn update(&mut self) -> Result<()> {
        self.update_with_mutate(|_| {})
    }

    pub fn update_with_mutate(&mut self, f: impl Fn(&mut Touch)) -> Result<()> {
        if self.paused {
            return Ok(());
        }
        let next = self.scenes.last_mut().unwrap().next_scene(&mut self.tm);
        /* 【话梅 2026-11-17】任何场景切换都清空触摸状态。
           场景切换往往发生在「点了一个按钮」之后，手指刚抬起；
           如果那一刻的 Ended 被吞掉（UI 层拦截、焦点切换、进程挂起），
           触点就会残留，回来后在谱面库表现为「短按触发长按菜单」
           「同时命中好几个谱面」。见 judge::request_touch_reset。 */
        if !matches!(next, NextScene::None) {
            crate::judge::request_touch_reset();
        }
        match next {
            NextScene::None => {}
            NextScene::Pop => {
                self.scenes.pop();
                self.tm.seek_to(self.times.pop().unwrap());
                self.scenes.last_mut().unwrap().enter(&mut self.tm, self.target_chooser.choose())?;
            }
            NextScene::PopN(num) => {
                for _ in 0..num {
                    self.scenes.pop();
                    self.tm.seek_to(self.times.pop().unwrap());
                }
                self.scenes.last_mut().unwrap().enter(&mut self.tm, self.target_chooser.choose())?;
            }
            NextScene::PopWithResult(result) => {
                self.scenes.pop();
                self.tm.seek_to(self.times.pop().unwrap());
                self.scenes.last_mut().unwrap().on_result(&mut self.tm, result)?;
                self.scenes.last_mut().unwrap().enter(&mut self.tm, self.target_chooser.choose())?;
            }
            NextScene::PopNWithResult(num, result) => {
                for _ in 0..num {
                    self.scenes.pop();
                    self.tm.seek_to(self.times.pop().unwrap());
                }
                self.scenes.last_mut().unwrap().on_result(&mut self.tm, result)?;
                self.scenes.last_mut().unwrap().enter(&mut self.tm, self.target_chooser.choose())?;
            }
            NextScene::Exit => {
                self.should_exit = true;
            }
            NextScene::Overlay(mut scene) => {
                self.times.push(self.tm.now());
                scene.enter(&mut self.tm, self.target_chooser.choose())?;
                self.scenes.push(scene);
            }
            NextScene::Replace(mut scene) => {
                scene.enter(&mut self.tm, self.target_chooser.choose())?;
                *self.scenes.last_mut().unwrap() = scene;
            }
        }
        Judge::on_new_frame();
        let mut touches = Judge::get_touches();
        touches.iter_mut().for_each(f);
        if !(touches.is_empty() || FULL_LOADING.with(|it| it.borrow().is_some())) {
            let now = self.tm.now();
            let delta = (now - self.last_update_time) / touches.len() as f64;
            let start_time = self.tm.start_time;
            let mut last_err = None;
            DIALOG.with(|it| -> Result<()> {
                let mut index = 1;
                touches.retain_mut(|touch| {
                    let t = self.last_update_time + (index + 1) as f64 * delta;
                    index += 1;
                    let mut guard = it.borrow_mut();
                    if let Some(dialog) = guard.as_mut() {
                        if !dialog.touch(touch, t as _) {
                            drop(guard);
                            *it.borrow_mut() = None;
                        }
                        false
                    } else {
                        drop(guard);
                        self.tm.seek_to(t);
                        match self.scenes.last_mut().unwrap().touch(&mut self.tm, touch) {
                            Ok(val) => !val,
                            Err(err) => {
                                warn!(?err, "failed to handle touch");
                                last_err = Some(err);
                                false
                            }
                        }
                    }
                });
                Ok(())
            })?;
            if let Some(err) = last_err {
                return Err(err);
            }
            self.tm.start_time = start_time;
        }
        self.touches = Some(touches);
        self.last_update_time = self.tm.now();
        DIALOG.with(|it| {
            if let Some(dialog) = it.borrow_mut().as_mut() {
                dialog.update(self.last_update_time as _);
            }
        });
        self.scenes.last_mut().unwrap().update(&mut self.tm)?;
        Ok(())
    }

    pub fn render(&mut self, painter: &mut TextPainter) -> Result<()> {
        if self.paused {
            return Ok(());
        }
        /* 【话梅 2026-11-17】全局 TAA：整个 UI 先画进离屏纹理，
           再按「当前帧 w + 历史 1-w」叠到屏幕。

           为什么必须提到这一层：原来 TAA 只在 GameScene 的 chart_target
           路径里做，大厅/设置页/谱面库没有离屏目标，所以设置里改 TAA
           在大厅完全看不出变化（用户实测反馈）。

           为什么用全局原子开关而不是读 config：Main 属于 prpr，
           拿不到 phira 管的 config。phira 在启动与改设置时写入
           taa::set_global_enabled(config.taa)。 */
        let dim = (screen_width() as u32, screen_height() as u32);
        let want_taa = crate::taa::global_enabled()
            && dim.0 > 0
            && dim.1 > 0
            && !self.scenes.last().is_some_and(|it| it.handles_own_aa());

        let taa_jitter = if want_taa {
            if self.taa_dim != dim || self.taa_target.is_none() {
                if let Some(old) = self.taa_target.take() {
                    old.delete();
                }
                self.taa_target = Some(render_target(dim.0, dim.1));
                self.taa_dim = dim;
                // 尺寸变了历史就作废，否则会采到尺寸不符的旧画面
                self.taa.reset();
            }
            self.taa.ensure_history(dim);
            Some(self.taa.jitter(dim))
        } else {
            None
        };

        let target = if want_taa { self.taa_target } else { None };
        let mut ui = Ui::new_with_target(painter, self.viewport, target);

        /* 亚像素抖动：加在 UI 变换矩阵的平移量上。
           不走相机 —— 场景（尤其 GameScene）自己会 set_camera，
           改了会被覆盖，还会和它自己的 jitter 叠加成双倍抖动。
           UI 顶点空间是 [-1,1]，而 jitter 的返回值正是这个空间里的
           半像素偏移，所以直接左乘平移矩阵即等效。 */
        if let Some(j) = taa_jitter {
            ui.transform = nalgebra::Translation2::new(j.x, j.y).to_homogeneous() * ui.transform;
        }

        ui.set_touches(self.touches.take());
        ui.scope(|ui| self.scenes.last_mut().unwrap().render(&mut self.tm, ui))?;
        if self.top_level {
            push_camera_state();
            set_camera(&ui.camera());
            let mut gl = unsafe { get_internal_gl() };
            gl.flush();
            // gl.quad_gl.render_pass(None);
            // gl.quad_gl.viewport(None);
            BILLBOARD.with(|it| {
                let mut guard = it.borrow_mut();
                let t = guard.1.now() as f32;
                guard.0.render(&mut ui, t);
            });
            DIALOG.with(|it| {
                if let Some(dialog) = it.borrow_mut().as_mut() {
                    dialog.render(&mut ui, self.tm.now() as _);
                }
            });
            let remove = FULL_LOADING.with(|it| {
                if let Some(loading) = it.borrow_mut().as_mut() {
                    if Arc::strong_count(&loading.keep_alive) > 1 {
                        if let Some(text) = loading.text.as_ref() {
                            ui.full_loading(text.clone(), self.tm.now() as _);
                        } else {
                            ui.full_loading_simple(self.tm.now() as _);
                        }
                        return false;
                    } else {
                        return true;
                    }
                }
                false
            });
            if remove {
                FULL_LOADING.take();
            }
            pop_camera_state();
        }

        /* 【话梅 2026-11-17】TAA 收尾：把离屏内容叠到屏幕，并写入历史。
           顺序与 game.rs 里那份保持一致，踩过的坑也写在那儿：
           先画当前帧（不透明），再以 alpha=1-w 叠历史 —— 反了就是历史盖住当前。 */
        if let (true, Some(t)) = (want_taa, self.taa_target) {
            unsafe { get_internal_gl() }.flush();
            push_camera_state();
            set_camera(&Camera2D {
                zoom: vec2(1., 1.),
                ..Default::default()
            });

            let w = self.taa.history_weight(0.);
            let hist = self.taa.history();
            draw_texture_ex(
                t.texture,
                -1.,
                -1.,
                WHITE,
                DrawTextureParams {
                    dest_size: Some(vec2(2., 2.)),
                    ..Default::default()
                },
            );
            if let Some(h) = hist {
                draw_texture_ex(
                    h,
                    -1.,
                    -1.,
                    Color { a: 1. - w, ..WHITE },
                    DrawTextureParams {
                        dest_size: Some(vec2(2., 2.)),
                        ..Default::default()
                    },
                );
            }
            pop_camera_state();

            /* 把当前帧写进历史。少了这步历史永远是空的，
               下一帧混合就什么都没叠上（看起来像没生效）。 */
            if let Some(h) = self.taa.history_target() {
                push_camera_state();
                set_camera(&Camera2D {
                    zoom: vec2(1., 1.),
                    render_target: Some(h),
                    ..Default::default()
                });
                draw_texture_ex(
                    t.texture,
                    -1.,
                    -1.,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(vec2(2., 2.)),
                        ..Default::default()
                    },
                );
                pop_camera_state();
            }
            self.taa.advance();
        }
        Ok(())
    }

    pub fn pause(&mut self) -> Result<()> {
        self.paused = true;
        self.scenes.last_mut().unwrap().pause(&mut self.tm)
    }

    pub fn resume(&mut self) -> Result<()> {
        self.paused = false;
        self.scenes.last_mut().unwrap().resume(&mut self.tm)
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }
}

fn draw_background(tex: Texture2D) {
    draw_background_asp(tex, screen_aspect());
}

/// 用**调用方给的**比例铺满整屏。
///
/// `screen_aspect()` 取的是 quad_gl 当前的视口 —— 它可能是**上一个场景**留下的
/// （结算场景的相机是另设的）。比例一旦不对，背景就只铺到某一行，
/// 屏幕下方出现一条硬边（2026-11-16 实拍：约 92% 高度处一条横线）。
/// 场景自己知道自己的 asp，直接传进来最可靠。
pub(crate) fn draw_background_asp(tex: Texture2D, asp: f32) {
    let top = 1. / asp;
    draw_image(tex, Rect::new(-1., -top, 2., top * 2.), ScaleType::CropCenter);
    draw_rectangle(-1., -top, 2., top * 2., Color::new(0., 0., 0., 0.3));
}

pub type LocalSceneTask = LocalTask<Result<NextScene>>;

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_dialog_text;

    /// 单行：逐字节复现 UTF-16 污染（BOM → 两个 U+FFFD；每个 ASCII 后跟一个 NUL）
    #[test]
    fn repairs_utf16_read_as_utf8() {
        let mut polluted = String::from("\u{FFFD}\u{FFFD}");
        for c in "pyrpansiyuan123456@qq.com".chars() {
            polluted.push(c);
            polluted.push('\0');
        }
        polluted.push_str("\r\0\n\0");
        assert_eq!(sanitize_dialog_text(&polluted, false), "pyrpansiyuan123456@qq.com");
    }

    /// 单行：**子进程噪声粘在前面**。用 `data/huamei-login.log` 里的真实数据做用例。
    #[test]
    fn drops_process_noise_before_value() {
        let raw = "[WARNING] Ignore caches that are heterogeneous\r\npyzpansiyuan123456@qq.com";
        assert_eq!(sanitize_dialog_text(raw, false), "pyzpansiyuan123456@qq.com");

        // 多行噪声 + 空行
        let raw2 = "line one\nline two\n\n  123456  \n";
        assert_eq!(sanitize_dialog_text(raw2, false), "123456");

        // 干净的单行输入原样通过
        assert_eq!(sanitize_dialog_text("123456", false), "123456");
        assert_eq!(sanitize_dialog_text("hello world", false), "hello world");
    }

    /// ⚠ 多行模式**绝不能**切行 —— 用户的内容本身就有换行
    #[test]
    fn multiline_is_never_split() {
        let user = "第一行\n第二行\n第三行";
        assert_eq!(sanitize_dialog_text(user, true), "第一行\n第二行\n第三行");
        // 只收尾（PowerShell 追加的换行），不动中间
        assert_eq!(sanitize_dialog_text("a\r\nb\r\n", true), "a\r\nb");
        // 多行里的 NUL 污染照样清
        assert_eq!(sanitize_dialog_text("a\0b\nc\0d", true), "ab\ncd");
    }

    /// 干净串要原样返回（别误伤）
    #[test]
    fn passthrough_when_clean() {
        assert_eq!(sanitize_dialog_text("", false), "");
        assert_eq!(sanitize_dialog_text("正常中文 English 123", false), "正常中文 English 123");
    }
}
