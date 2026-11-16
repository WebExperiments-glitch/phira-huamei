//! huamei 数据飞轮 —— 对局记录的结构与紧凑编码。
//!
//! 【为什么放在 prpr 而不是 phira】
//!   逐音符判定只存在于 `Judge` 里，而 `Judge` 属于 prpr。prpr 又不能反向依赖
//!   phira（网络层在那边）。所以**数据**在 prpr 组装，**发送**在 phira 完成：
//!   prpr 把 `HuameiPayload` 序列化成 JSON 字节交给既有的 `UploadFn`，
//!   phira 侧补上谱面摘要再 POST。
//!
//! 【零改动复用上游管道】
//!   上游已经有一条「结算 → EndingScene → upload_fn(Vec<u8>)」的通路，
//!   只是开源构建里整段被 `#[cfg(closed)]` 关掉了（`game.rs:948`）。
//!   我们把它打开、换成自己的载荷格式，管道本身一行不改。
//!
//! 契约见 `docs/规范/数据飞轮契约.md`。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::judge::{Judge, Judgement, PlayResult};

/// 判定数据格式版本。改编码必须 +1（服务端按这个字段选解析器）。
pub const JUDGE_VER: u8 = 2;   /* 2 = 加入 max_touch_points（2026-11-14） */

/// 紧凑记录里的判定码
pub mod judge_code {
    pub const PERFECT: u8 = 0;
    pub const GOOD: u8 = 1;
    pub const BAD: u8 = 2;
    pub const MISS: u8 = 3;
    /// Hold 的「按下」事件 —— **不是失误**，只表示这一次按到了长条
    pub const HOLD_PRESS: u8 = 4;
}

/// `offset_ms` = 这个值表示**上游没记录真实偏移**，不是 0ms。
///
/// 三条来源（对得上 `prpr/src/judge.rs:827-839`）：
///   · Miss      → 上游硬编码 `0.25`（哨兵，不是偏移）
///   · Drag/Flick→ 上游硬编码 `0.`（这些音符本来就自动判 Perfect）
///   · Hold 按下 → `Err(_)` 分支没有 diff
/// 把这个区分做出来，服务端算「平均早/晚」时才不会把垃圾算进去。
pub const OFFSET_UNKNOWN: i16 = i16::MIN;

/// 一条紧凑判定记录占 8 字节（小端）：
/// ```text
/// +0  u16  line_id     `chart.lines` 的下标
/// +2  u16  note_id     该线内 `notes` 的下标
/// +4  i16  offset_ms   早/晚毫秒；OFFSET_UNKNOWN 表示未知
/// +6  u8   judgement   judge_code::*
/// +7  u8   flags       保留，填 0
/// ```
pub const RECORD_SIZE: usize = 8;

/// 上报载荷。字段名走 snake_case（服务端是 JS，无所谓，但保持一致好排查）。
/// 设备与系统信息 —— 上报载荷的一部分。
///
/// ⚠ 每个字段都可空，理由不同：
///   · os_version / model / screen：iOS 上拿得到，但其它平台采集方式不同
///   · lat / lon：要用户授权定位，拒绝就是 None
///   · gyro：要 CoreMotion，且只传摘要（完整采样太大）
/// 拒绝授权不阻塞上报 —— 采不到就 null，数据照收。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 系统版本串，如 "17.0" 或 "iOS 17.0 (21A329)"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// 机型标识，如 "iPhone15,2"（不是营销名）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 屏幕逻辑尺寸 [宽, 高] 与像素比
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<[f32; 3]>,
    /// 城市级经纬度（已降精度）。None = 未授权或未取到。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    /// 陀螺仪摘要：[均值, 标准差, 峰值]，单位 rad/s。
    /// 传摘要而不是完整采样：一局 3 分钟 @60Hz 是 1 万个点，没人会分析。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gyro: Option<[f32; 3]>,
    /// **实际渲染尺寸** [宽, 高]，单位像素。
    ///
    /// 【为什么不能只看 `screen`】
    ///   `screen` 来自 `UIScreen`，是**设备的屏幕**，不是这一局实际渲染的区域。
    ///   iPhone Duo 是双屏设备（内屏 7.6" / 外屏 5.4"），`UIScreen` 在它上面
    ///   语义不明确，而且分屏 / 窗口化时也和渲染区不一致。
    ///   这个字段取的是绘制缓冲的真实尺寸，配合 `model` 就能反推姿态
    ///   （内屏 / 外屏 / 横竖），是 Duo 数据能分层的唯一依据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<[f32; 2]>,
}

/* 全局设备信息 —— 由 phira 侧在启动时采集一次并写入。

   ⚠ 为什么不用参数传：build_payload 的调用点在 prpr 的 game.rs 里，
     而采集需要 objc2-ui-kit（只有 phira 依赖它）。直接传要穿透
     Scene → LoadingScene → GameScene 三层构造函数，改动面远大于收益。

   采不到（非 iOS / 权限拒绝 / 还没设）就保持 Default，序列化时字段省略。 */
static DEVICE_INFO: std::sync::OnceLock<DeviceInfo> = std::sync::OnceLock::new();

/// 由 phira 侧调用一次。重复调用以第一次为准（OnceLock 语义）。
pub fn set_device_info(info: DeviceInfo) {
    let _ = DEVICE_INFO.set(info);
}

/// 取当前设备信息。未设置时返回 Default（所有字段 None）。
pub fn device_info() -> DeviceInfo {
    DEVICE_INFO.get().cloned().unwrap_or_default()
}

/* ── 平台能力注册表 ──────────────────────────────────────────
   prpr 不认识 UIKit / CoreMotion / CLLocation，也**不能**依赖 phira
   （那会循环依赖）。所以平台侧的能力从这里注入。

   【为什么是一个结构体，而不是 5 个 set_xxx_hook】
     · 以前每个钩子各有一个 AtomicPtr + setter + unsafe transmute（5 份），
       注册点散在 device.rs 里 —— **加了钩子忘了注册就是运行时静默空操作**。
       现在构造 `PlatformHooks` 时少写一个字段直接编译失败。
     · 5 处 `unsafe { transmute }` 全部消失，只剩普通函数调用。
     · 「没装」这件事只在一个地方体现（`hooks()` 返回 None），
       不用再去猜某个能力到底是没实现还是没注册。 */
pub struct PlatformHooks {
    /// 每帧采一帧陀螺仪
    pub sample_motion: fn(),
    /// 对局开始（含重开）时清零采样器
    pub begin_round: fn(),
    /// 结算时取本局摘要 [均值, 标准差, 峰值]，rad/s
    pub motion_summary: fn() -> Option<[f32; 3]>,
    /// 城市级坐标 [lat, lon]；未授权 / 未定上位为 None
    pub location: fn() -> Option<[f64; 2]>,
    /// 安全区 (上, 左, 下, 右)，像素
    pub safe_area: fn() -> (f32, f32, f32, f32),
    /// 写一行诊断到可导出的地方（iOS 上是 Documents/motion.log）。
    /// prpr 的 tracing 在 iOS 只走 NSLog —— 没有 Mac 就读不到，
    /// 涉及渲染参数的排查必须走这条路。
    pub log_line: fn(&str),
}

/// 写一条诊断。未注册时退化成 tracing（桌面端照常输出）。
pub fn diag(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    match hooks() {
        Some(h) => (h.log_line)(msg),
        None => tracing::info!("{msg}"),
    }
}

static HOOKS: std::sync::OnceLock<PlatformHooks> = std::sync::OnceLock::new();

/// 由 phira 侧在启动时调用一次。重复调用以第一次为准。
pub fn install_hooks(h: PlatformHooks) {
    let _ = HOOKS.set(h);
}

fn hooks() -> Option<&'static PlatformHooks> {
    HOOKS.get()
}

/// 每帧调用一次。未注册时是空操作。
pub fn sample_motion() {
    if let Some(h) = hooks() {
        (h.sample_motion)();
    }
}

/// 对局开始时调用（含重开）。未注册时是空操作。
pub fn begin_round_motion() {
    if let Some(h) = hooks() {
        (h.begin_round)();
    }
}

/// 取本局的陀螺仪摘要。未注册或没采到都返回 None。
pub fn motion_summary() -> Option<[f32; 3]> {
    hooks().and_then(|h| (h.motion_summary)())
}

/// 当前城市级坐标。未注册或未授权都返回 None。
pub fn location_coord() -> Option<[f64; 2]> {
    hooks().and_then(|h| (h.location)())
}

/// 当前安全区（上, 左, 下, 右），单位像素。未注册或取不到时全 0。
pub fn safe_area() -> (f32, f32, f32, f32) {
    hooks().map(|h| (h.safe_area)()).unwrap_or((0., 0., 0., 0.))
}

/// 把整屏视口按安全区**左右**缩进。
///
/// 【为什么只缩左右，不缩上下】
///   上下那部分（iPad 的 24pt 状态栏、全面屏的 Home 指示条）从 0.8.2 起就一直
///   是画在下面的，玩家已经按那个比例练出手感 —— 动了就是「改表现」而不是
///   「修 bug」（交接文档第五节的第 3 条坑）。
///   而 Duo 的竖排状态栏是**真的压在轨道上**：横屏时它占住最外侧 84pt，
///   音符会被时钟盖住。所以只处理这一侧边距。
pub fn safe_viewport(vp: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (_, l, _, r) = safe_area();
    let (x, y, w, h) = vp;
    let (l, r) = (l.round().max(0.) as i32, r.round().max(0.) as i32);
    /* 护栏：收缩量不能超过一半。取到离谱的值（系统换了、单位算错）时
       宁可让 UI 压在时钟上，也不能把视口吃成一条缝。 */
    if l + r == 0 || w - l - r < w / 2 {
        return vp;
    }
    (x + l, y, w - l - r, h)
}

/* ── 触控落点热度图 ──────────────────────────────────

   【为什么不是完整轨迹】
     一局 600 个音符、每帧 2~4 个触点、60fps → 几十万个坐标点。
     传上去没人能分析，存储也吃不消。

   【8×8 网格能回答什么】
     · 玩家主要按屏幕上半区还是下半区（谱面读法习惯）
     · 是否偏一侧（右手/左手优势）
     · 与判定偏移联合看：左下角区域是不是普遍偏早
   64 个计数归一化到 u8，base64 后仅 88 字符。

   ⚠ 只记**触点按下那一刻**（Started），不记移动中的帧 ——
     否则按住不动的手指会被重复计数几千次，热度图直接失真。 */

const HEAT_N: usize = 8;
static TOUCH_HEAT: [std::sync::atomic::AtomicU32; HEAT_N * HEAT_N] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    [ZERO; HEAT_N * HEAT_N]
};

/// 记一个触点。`x` / `y` 是归一化坐标（0..1，左上为原点）。
/// 越界直接丢弃。
pub fn record_touch(x: f32, y: f32) {
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return;
    }
    let cx = ((x * HEAT_N as f32) as usize).min(HEAT_N - 1);
    let cy = ((y * HEAT_N as f32) as usize).min(HEAT_N - 1);
    TOUCH_HEAT[cy * HEAT_N + cx].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// 清空（对局开始时调）。
pub fn reset_touch_heat() {
    for c in TOUCH_HEAT.iter() {
        c.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 取出并归一化成 [u8; 64]。全 0 时返回 None（没采到触控）。
pub fn touch_heatmap() -> Option<[u8; HEAT_N * HEAT_N]> {
    let raw: Vec<u32> = TOUCH_HEAT.iter().map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).collect();
    let max = raw.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return None;
    }
    let mut out = [0u8; HEAT_N * HEAT_N];
    for (i, v) in raw.iter().enumerate() {
        // 线性归一化到 0..255 —— 形状比绝对次数重要
        out[i] = ((*v as f32 / max as f32) * 255.0).round() as u8;
    }
    Some(out)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HuameiPayload {
    pub client_ver: String,
    pub judge_ver: u8,

    pub chart_id: i32,
    pub chart_level: String,
    /// sha256(谱面文件字节) —— 与 Phira 官方 `/chart/{id}/verify-cksum` 同一个算法。
    /// ⚠ 由 **phira 侧**在谱面加载时算好填入（prpr 拿不到原始谱面字节）。
    /// 没有就**不出现**这个键（而不是 `null`）—— 与旧客户端的上报字节保持一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart_digest: Option<String>,
    pub chart_notes: u32,

    pub score: u32,
    pub accuracy: f64,
    pub max_combo: u32,
    /// [perfect, good, bad, miss]
    pub counts: [u32; 4],
    pub speed: f32,
    pub mods: i32,
    /// 是否开着键盘模式。不拒收（桌面端键盘是正当玩法），但服务端可以据此分层。
    pub keyboard: bool,

    /// 本局最大同时触点数。0 = 键盘模式或旧客户端；1 = 单指；2~4 = 双指/多指。
    /// ⚠ 行为特征，不是成绩 —— 同样 100 万分的两个人，指法结构可能完全不同。
    /// serde(default) 让 judge_ver=1 的历史数据仍能反序列化。
    #[serde(default)]
    pub max_touch_points: u8,

    /// 结算时刻，Unix 毫秒
    pub played_at: i64,

    /// base64(紧凑判定块)
    pub judge: String,

    /// 设备与系统信息。**全部可空** —— 权限被拒或平台不支持时留 None。
    /// 用途是分层分析（机型性能 vs 判定偏差），不是用户画像。
    #[serde(default)]
    pub device: DeviceInfo,

    /// base64(8×8 落点热度图，64 字节，已归一化到 0..255)。
    /// None = 纯键盘玩法，没有触控数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touch_heat: Option<String>,
}

impl HuameiPayload {
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// 把 `Judge` 里的逐音符判定编成紧凑块。
///
/// ⚠ `judgements` 的第 4 个字段是 `Result<JudgeHit, bool>`：
///   `Ok(hit)`  = 真判定（记 `hit.judgement` 与 `hit.diff`）
///   `Err(b)`   = **Hold 的按下事件**（`b` = 是否落在 Perfect 窗口）
/// 把 `Err` 当失误是最容易犯的错 —— 那会让每个 Hold 都污染热力图。
pub fn encode_judgements(judge: &Judge) -> Vec<u8> {
    let js = judge.judgements.borrow();
    let mut out = Vec::with_capacity(1 + js.len() * RECORD_SIZE);
    out.push(JUDGE_VER);

    for (line_id, note_id, res) in js.iter().map(|it| (it.1, it.2, &it.3)) {
        let (code, offset) = match res {
            Ok(hit) => {
                let code = match hit.judgement {
                    Judgement::Perfect => judge_code::PERFECT,
                    Judgement::Good => judge_code::GOOD,
                    Judgement::Bad => judge_code::BAD,
                    Judgement::Miss => judge_code::MISS,
                };
                (code, offset_to_ms(hit.judgement, hit.diff))
            }
            // Hold 按下：没有偏移
            Err(_) => (judge_code::HOLD_PRESS, OFFSET_UNKNOWN),
        };

        // line_id / note_id 超出 u16 是不可能的（谱面不会有 6.5 万条判定线），
        // 但仍然夹一下，宁可丢信息也不要写出越界数据。
        out.extend_from_slice(&(line_id.min(u16::MAX as u32) as u16).to_le_bytes());
        out.extend_from_slice(&(note_id.min(u16::MAX as u32) as u16).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.push(code);
        out.push(0); // flags 保留
    }
    out
}

/// `diff`（秒）→ 毫秒。
///
/// ⚠ 只有 **Click / Hold** 的 `diff` 是真实偏移；Drag / Flick 上游硬编码 `0.`
/// （`judge.rs:834`），Miss 是 `0.25` 哨兵（`judge.rs:832`）。
/// Drag / Flick 的 `0.` 无法与「真的正中」区分 —— 但服务端拿着谱面副本，
/// 知道哪些 note 是 Drag/Flick，统计时排除即可，所以这里保留 0 不误标为未知。
fn offset_to_ms(judgement: Judgement, diff: f64) -> i16 {
    if matches!(judgement, Judgement::Miss) || !diff.is_finite() {
        return OFFSET_UNKNOWN;
    }
    (diff * 1000.).round().clamp(i16::MIN as f64 + 1., i16::MAX as f64) as i16
}

/// 组装载荷。
///
/// ⚠ `client_ver` 留空、`chart_digest` 留 `None` —— 这两项**由 phira 侧填**
///   （它才知道自己的版本号，也只有它拿得到谱面原始字节）。phira 会反序列化
///   这个载荷、补上两项、再序列化发出去。见 `phira/src/scene/song.rs`。
#[allow(clippy::too_many_arguments)]
pub fn build_payload(
    chart_id: i32,
    chart_level: &str,
    chart_notes: u32,
    result: &PlayResult,
    speed: f32,
    mods: i32,
    keyboard: bool,
    judge_bytes: Vec<u8>,
    device: DeviceInfo,
) -> HuameiPayload {
    HuameiPayload {
        client_ver: String::new(),
        judge_ver: JUDGE_VER,
        chart_id,
        chart_level: chart_level.to_owned(),
        chart_digest: None,
        chart_notes,
        score: result.score,
        accuracy: result.accuracy,
        max_combo: result.max_combo,
        counts: result.counts,
        speed,
        mods,
        keyboard,
        max_touch_points: if keyboard { 0 } else { result.max_touch.min(255) as u8 },
        played_at: chrono::Utc::now().timestamp_millis(),
        judge: STANDARD.encode(&judge_bytes),
        /* 陀螺仪摘要填进 device.gyro —— 它是「这一局的身体状态」，
           与环境信息同属 device 概念，不单开字段。 */
        device: {
            let (lat, lon) = match location_coord() {
                Some([a, b]) => (Some(a), Some(b)),
                None => (None, None),
            };
            DeviceInfo {
                gyro: motion_summary(),
                lat,
                lon,
                /* 实际渲染尺寸：这一局真正画了多少像素。
                   与 `screen`（设备屏幕）不同，它反映当前姿态与窗口，
                   是 Duo 内外屏 / 分屏数据能分层的依据。 */
                view: Some([macroquad::window::screen_width(), macroquad::window::screen_height()]),
                ..device
            }
        },
        touch_heat: touch_heatmap().map(|h| STANDARD.encode(h)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上报契约：这些键**必须**出现在序列化结果里。
    ///
    /// 【为什么需要这条测试】
    ///   这个载荷曾经有两套结构体（prpr 的 `HuameiPayload` 和 phira 手写的
    ///   `UploadBody`），加字段只改一处 ⇒ 客户端"传了"、服务端收到 null、
    ///   全程无报错。`max_touch_points` 和 `device` 各踩过一次。
    ///
    ///   现在 phira 直接上传 `HuameiPayload`，结构体本身就唯一了；这条测试
    ///   再钉住"这些键不能消失"。服务端 `lib/hm.mjs` 的 validatePlay 认的就是
    ///   这批名字，改服务端字段时两边一起改。
    #[test]
    fn payload_keeps_wire_contract() {
        let p = HuameiPayload {
            client_ver: "x".into(),
            judge_ver: JUDGE_VER,
            chart_id: 1,
            chart_level: "IN 1".into(),
            chart_digest: Some("d".into()),
            chart_notes: 2,
            score: 3,
            accuracy: 4.0,
            max_combo: 5,
            counts: [1, 0, 0, 0],
            speed: 6.0,
            mods: 0,
            keyboard: false,
            max_touch_points: 2,
            played_at: 7,
            judge: "j".into(),
            device: DeviceInfo::default(),
            touch_heat: Some("h".into()),
        };
        let v: serde_json::Value = serde_json::from_slice(&p.to_json_bytes().unwrap()).unwrap();
        for k in [
            "client_ver",
            "judge_ver",
            "chart_id",
            "chart_level",
            "chart_digest",
            "chart_notes",
            "score",
            "accuracy",
            "max_combo",
            "counts",
            "speed",
            "mods",
            "keyboard",
            "max_touch_points",
            "played_at",
            "judge",
            "device",
            "touch_heat",
        ] {
            assert!(v.get(k).is_some(), "上报载荷丢了字段：{k}");
        }
    }

    #[test]
    fn offset_clamped_and_miss_is_unknown() {
        assert_eq!(offset_to_ms(Judgement::Perfect, 0.0123), 12);
        assert_eq!(offset_to_ms(Judgement::Good, -0.0456), -46);
        // Miss 的 0.25 哨兵不能被当成 250ms
        assert_eq!(offset_to_ms(Judgement::Miss, 0.25), OFFSET_UNKNOWN);
        // 极端值被钳住
        assert_eq!(offset_to_ms(Judgement::Bad, 99.0), i16::MAX);
        assert_eq!(offset_to_ms(Judgement::Bad, f64::NAN), OFFSET_UNKNOWN);
    }

    #[test]
    fn judge_code_is_stable() {
        // 这些数字是**线上协议**，改动等于破坏兼容
        assert_eq!(judge_code::PERFECT, 0);
        assert_eq!(judge_code::GOOD, 1);
        assert_eq!(judge_code::BAD, 2);
        assert_eq!(judge_code::MISS, 3);
        assert_eq!(judge_code::HOLD_PRESS, 4);
        assert_eq!(JUDGE_VER, 2);
        assert_eq!(RECORD_SIZE, 8);
    }
}
