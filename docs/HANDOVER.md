# Huamei Phira 交接文档

> 2026-11-15 · 交接人：乌梅干 / DeepSeek V4.1 Flash 1017
> 接手人：**待定**
> 状态：**功能可用，有已知缺陷，方向明确**

---

## 一、这个东西是干嘛的

**一句话**：魔改版 Phira 客户端 + 一个数据接收站，把玩家**每一局真实打谱的行为数据**收集起来，用来做难度模型。

**为什么需要它**（这是整件事的核心，不是技术选型）：

现有的难度模型用的是「官方定数」和「谱面特征」，两者都不够：

```
官方定数 vs 玩家表现    Pearson r = 0.478  (r² = 0.228)
50 参数线性模型          ρ = 0.8024
SSM 20 维               ρ = 0.8162   ← 配对检验不显著，模型不值钱
```

**官方定数解释不了难度**（r² 只有 0.23），而换模型架构也没用——真正的瓶颈是**缺一类数据：人类怎么打这张谱**。

一张 1028 个音符的谱，玩家打出来的 `943 Perfect / 35 Good / 10 Bad / 40 Miss` 是 **1028 次真实的生理测量**。每次判定都告诉你「他在这里跟得上 / 跟不上」。

**这个客户端就是为了拿到这个数据。**

---

## 二、当前架构

```
┌─────────────────┐        ┌──────────────────┐         ┌──────────┐
│  Phira 客户端    │  POST  │  phm-web 服务端   │  异步   │  Neon    │
│  (phira-huamei) │ ─────> │  /api/hm/play    │ ──────> │ Postgres │
│                 │  202   │  (队列 + 重试)    │         │          │
└─────────────────┘        └──────────────────┘         └──────────┘
        │                          │
        │ 逐音符判定 (base64)        │ 体检：tools/hm-stats.mjs
        │ 设备/定位/陀螺仪/触控热度    │ 站点：phm.app.workbuddy.host
        └──────────────────────────┘
```

**两个仓库**：

| 仓库 | 作用 |
|---|---|
| `WebExperiments-glitch/phira-huamei` | 魔改客户端（Rust / macroquad） |
| `WebExperiments-glitch/Phira-Huamei-Metric` | 服务端 + 站点（Node） |

**上游**：`TeamFlos/phira`（我们 fork 自 `main`，非 release 分支）

---

## 三、已完成的东西（2026-11-15）

### 服务端

**① `hm_plays` 表的字段扩充**

| 列 | 内容 | 备注 |
|---|---|---|
| `judge_blob` | 逐音符判定，base64 | 每音符 2 字节：判定码 + 偏移 |
| `chart_digest` | 谱面内容哈希 | **A 方向的生命线**——同谱改了版本就不可比 |
| `device` (jsonb) | 机型 / 系统 / 屏幕 / 城市级坐标 / 陀螺仪摘要 | 见下 |
| `max_touch_points` | 本局最大同时触点数 | null=旧客户端，0=键盘 |
| `touch_heat` | 8×8 落点热度图，base64 64 字节 | 归一化到 0..255 |

`device` 的结构：

```json
{
  "os_version": "27.2.1",
  "model": "iPad14,4",
  "screen": [834, 1194, 2],
  "lat": 31.16, "lon": 120.62,      // 城市级，2 位小数 ≈1km
  "gyro": [mean, std, peak]          // rad/s，未采到则无此键
}
```

**② 异步队列（`lib/hm-queue.mjs`）**

这是今天最重要的改动。原来 `/api/hm/play` 要等数据库写完才返回，而 Neon 免费档**空闲 5 分钟就把 compute 缩到零**，玩家打完一局才上传一次，正好每次都撞冷启动（唤醒 10~13 秒）→ 30 秒连接超时 → 网关 `stgw` 返回 504。

现在：校验完就入队，**立刻返回 202**，后台 worker 串行写库、失败指数退避重试（1/2/4/8/16s，最多 5 次）。

**实测：1.47 秒返回**（此前 30 秒超时）。

**③ DB 保活（`lib/db-pg.mjs`）**

每 110 秒 `select 1`，比 5 分钟窗口留足余量。⚠ 必须 `unref()`，否则定时器吊住事件循环，进程退不掉。

**④ 幂等去重**

异步队列引入一个新失败模式：入队 → 202 → 进程重启 → 队列没了。客户端以为提交成功、删了本地记录，其实没落库。

所以加了唯一索引 `(chart_id, played_at, score)` + `hm_put_play_dedup()`，冲突时**静默返回已有 id**（`duplicate: true`）。

**⑤ 体检工具 `tools/hm-stats.mjs`**

```bash
cd phm-web && node tools/hm-stats.mjs           # 摘要
cd phm-web && node tools/hm-stats.mjs --verbose # 带明细
```

回答三个问题：同谱重复打了吗 / 判定完整吗 / 字段齐吗。

**为什么单独成工具**：之前每次临时写脚本查库，口径不一致——昨天说「完整性 100%」今天说「98%」，因为口径变了。固定下来才可比。

### 客户端

**① 逐音符判定采集**（`judge.rs` + `huamei.rs`）

`judge_ver = 2`。每个音符记 2 字节：判定码（perfect/good/bad/miss/hold_press）+ 偏移。

偏移有三条特殊来源（`offset_ms = 0.25` 表示「上游没记录真实偏移」，不是 0ms）：
- Miss → 上游硬编码 `0.25`（哨兵）
- Drag/Flick → 上游硬编码 `0.`（本来就自动判 Perfect）
- Hold 按下 → `Err(_)` 分支没有 diff

**② 设备与定位**（`device.rs` / `location.rs`）

- `device`：机型/系统/屏幕，通过 `objc2` 的 `NSProcessInfo` 和 `UIScreen` 读
- `location`：`CLLocationManager`，精度 1000m，**坐标在写入全局之前就四舍五入到 2 位小数**，原始值不活过那个函数
- **拒绝授权后恒为 None**，不重试、不二次弹窗

**③ 陀螺仪**（`motion.rs`）

对局期间每帧读 `CMMotionManager.deviceMotion.rotationRate` 的模长，结算算 `[均值, 标准差, 峰值]`。

**只传三个数**，不传采样序列——一局 3 分钟 @60Hz 是 1 万个点，没人分析。

**④ 触控落点热度图**（`huamei.rs` + `game.rs`）

8×8 网格 + 无锁 `AtomicU32` 计数，结算归一化到 u8 再 base64。

`game.rs` 每帧遍历 `Judge::get_touches()`，**只记 `TouchPhase::Started`**。记每一帧的话按住不动的手指会被算几千次，热度图彻底失真。

**⑤ 三个函数指针钩子**

`phira` 不能反向依赖 `prpr`，所以：

```rust
prpr::huamei::set_motion_hook(...)
prpr::huamei::set_motion_summary_hook(...)
prpr::huamei::set_location_hook(...)
```

`prpr` 定义入口（`AtomicPtr` 存函数指针），`phira` 注册实现。

---

## 四、未完成 / 已知缺陷（**接手必读**）

### 🔴 严重

**① 音频中断后没声音**（iOS）

`sasa` 的 iOS 后端走 `cpal`，而 **`cpal` 从不监听 `AVAudioSession` 的中断通知**。所以：
- 按下音量键 → 音频会话重配 → 掉帧
- 切后台再回来 → 会话被 deactivate，没人重新 activate → **没声音**
- 只有音乐没打击音 → 音乐流恢复了、SFX 流没恢复（两条流生命周期不同）

**修法**：在 `phira` 侧自己桥接 `NSNotificationCenter` 监听 `AVAudioSessionInterruptionNotification`，中断结束时 `setActive:YES` 并重建音频流。

**难点**：重建后 `music.position()` 会归零，得设计「恢复到哪个时间点」，否则歌曲从头开始。**这是设计决策，不是纯编码。**

**② `gyro` 一直是 null** —— **根因已定位（2026-11-15 晚），待真机验证**

不是 CoreMotion 的问题，是**我们自己的采样器从来没被启动过**：

`phira/src/motion.rs` 的 `begin_round()` 定义了，但**全仓库零调用**。
于是 `Sampler::running` 恒为 `false`，`push()` 每帧第一行就 `return`，
样本数永远是 0，`summarize()` 因 `< 10` 返回 `None`。

关键在于这是一次**完全静默的失败**：不报错、不 warning、字段就是 null，
和「设备没有陀螺仪」「授权被拒」在载荷里一模一样 —— 28 条数据 gyro 全空，
此前两版都在猜 CoreMotion 的启动时机，方向偏了。

修法（三处）：

- `prpr::huamei` 新增 `set_motion_begin_hook` / `begin_round_motion()`
- `game.rs` 的 `reset!` 宏里调用它 —— 每局开始（含重开）清零并置 `running=true`
- `phira::device::install` 注册 `motion::begin_round`

同时补了诊断：`isDeviceMotionAvailable` / `start` 之后的 `isDeviceMotionActive`
/ 首个样本帧号 / 结算时的 `frames + no_data + running + samples`。

⚠ **日志不再只看 NSLog**：Windows 上没有 Mac，NSLog 读不到，这是这个 bug
拖了两版的真正原因。现在同时写 `Documents/motion.log`（轮转上限 256 KB），
与 `panic.log` 同目录，「文件」App 可直接导出。

**③ 竖屏：打歌场景已重构（beta.30），菜单仍挤在右上角**

**已做（打歌场景铺满，不裁切）**

关键是把一个字段拆成两个：`aspect_ratio` 过去同时当「谱面比例」和「视图比例」，
因为两者恒等所以看不出问题。竖屏要铺满就非拆不可：

| 字段 | 含义 | 横屏 | 竖屏（iPad 11"） |
|---|---|---|---|
| `chart_aspect` | 谱面坐标 → 世界坐标的除数 | 1.43 | 1.43（= 横过来时的值） |
| `aspect_ratio` | 世界坐标 → 屏幕的相机缩放 | 1.43 | 0.698（= 屏幕比例） |

结果：竖屏音符像素大小、上下间距对应的谱面距离与横屏一致，但屏幕更高
⇒ **可视谱面范围 1.0 → 2.04，预读距离翻倍**。这就是竖屏该有的优势。

配套改动：
- `judge.rs` 的像素→谱面换算补了 `chart_aspect / aspect_ratio` 系数
  （横屏为 1，与旧行为逐字节一致；漏了它竖屏判定会整体偏移）
- `resource.rs::update_size` 按方向分派两套比例，横屏路径完全不变

**未做**
- **主界面 / 谱面库**（`phira/src/scene/main.rs`、`song.rs`）**还是挤在右上角**。
  原因：这两个场景**不持有 `prpr::core::Resource`**，拿不到 `update_size()`。
  2026-11-15 桌面竖屏窗口实测：菜单的可点区域和绘制位置**已经对不上**，
  竖屏下点不到「播放」按钮 —— 这不只是难看，是功能坏了。修法是把 `Resource`
  提到场景之上，结构性改动。
- **真正的竖屏谱面呈现**（音符按竖屏高度重新布局）没做，现在只是「把横屏
  谱面铺到竖屏上」。

### 🟡 中等

**④ `#828 新纪录显示不正常`**

只在 autoplay 时显示「新纪录 +xxx」。但 `ending.rs` 里只找到 `UNRATED` 的标记逻辑，**没有「新纪录」的代码**。可能在 `phira` 的 hud 层，或者这份 `main` 里根本没有这条文案。**得先定位。**

**⑤ `#866 打歌锁屏幕方向`**

macroquad 不暴露 `UIViewController`，而 iOS 锁方向必须走它（`supportedInterfaceOrientations` 或 iOS 16+ 的 `requestGeometryUpdate`）。没有 VC 引用，只能走 `UIDevice setValue:forKey:@"orientation"`——**私有 API，上架会被拒**。

**做不了，除非改 macroquad 的 iOS 后端加一个 VC 访问口。**

**⑥ iPhone Duo（折叠屏）适配** —— 2026-11-15 已改掉两处根因

`beta.28` 起处理了下面 ①②，③④ 仍待做。

参数（决定断点的就是这几个数）：内屏 7.6" 1878×2670 / **669×951pt**，
外屏 5.4" 1398×2034 / **466×678pt**，内外屏同比例（约 1.4:1），iOS 27。
注意它是**双屏设备**，不是「一块屏会变大小」。

**① 渲染尺寸不能取 `UIScreen.mainScreen`（已改）**

双屏设备上 mainScreen 语义不明确（Apple 明确要求改用 window scene 的 screen
或视图自身 bounds），且该 API 已标注待弃用。`vendor/prpr-miniquad/src/native/ios.rs`
的 `draw_in_rect_inner` 原来用 mainScreen.bounds 算 framebuffer 尺寸，现在改成
**GLKView 自身的 bounds**，取不到（首帧未布局）才退回 mainScreen。
副作用是顺带修好了 iPad 分屏 / Stage Manager 的窗口缩放。

**② 安全区（已接线，布局按左右缩进）**

Duo 内屏横屏与外屏把状态栏挪成**一侧的竖条**（约 84pt，放时钟/信号），
左右**不对称**；刘海机横屏同理。链路：

```
UIView.safeAreaInsets  →  miniquad::native::ios::safe_area_px()   （像素，每帧刷新）
                       →  prpr::huamei::set_safe_area_hook(...)   （phira 注册）
                       →  prpr::huamei::safe_viewport(vp)         （只缩左右）
                       →  game.rs 的 res.update_size(...)         （打歌视口）
```

**为什么只缩左右**：上下那部分（iPad 24pt 状态栏、Home 指示条）从 0.8.2 起
就一直画在下面，玩家已按那个比例练出手感；而 Duo 的竖排状态栏是**真的压在
轨道上**。改上下属于「改表现」，改左右属于「修 bug」。

每局结算会在 `motion.log` 记一行 `safe_area px t=… l=… b=… r=…`，
这是判断适配有没有生效的唯一凭据（无 Mac 环境读不到 NSLog）。

**③ 菜单与谱面库仍未适配（待做）**

`main.rs` / `song.rs` 不持有 `prpr::core::Resource`，拿不到 `update_size()`，
在 951×669 这种内屏横屏尺寸下仍会挤在右上角。修法是把 `Resource` 提到场景之上，
是结构性改动（见 ③ 竖屏适配那一条）——**和竖屏是同一个根治点**。

**④ 真正的竖屏玩法仍未做**

音符按竖屏高度生成、轨道重新布局。那不是适配，是重新设计谱面呈现。

**⑤ 在 Windows 上无法验证 Duo**

本机没有 iOS 模拟器（需要 Xcode 27.1 的 Device Hub），也没有真机。
`vendor/prpr-miniquad` 可以用
`cargo check -p miniquad --target aarch64-apple-ios` 单独编译校验；
`phira` 整体不行 —— `zstd-sys` 的构建脚本要 `xcrun`。
所以 Duo 的实际表现只能靠装到设备上的日志（`safe_area` 那一行）。

### 🟢 已完成但可改进

**⑦ `touch_heat` 丢了绝对次数**

目前归一化到 0..255（每格除以最大值 × 255），**形状保住了，但总点击数丢了**。这意味着没法比较两局的点击总量（「这张谱打了两遍，第二遍手势更省了吗」）。

下一版应该把**总点击数**单独存一个字段。

---

## 五、踩过的坑（**这些比代码值钱**）

### 1. 「两套结构体手工对齐」导致静默丢字段

`prpr/src/huamei.rs` 的 `HuameiPayload` 序列化出来是完整的，但 `phira/src/huamei.rs` 有个**独立的 `UploadBody`**，把数据**重新构造**了一遍。

加 `max_touch_points`（11-14）和 `device`（11-15）时，**都只改了前者**。结果：客户端传了、服务端收到 null、**全程无报错**（多传少传字段在 HTTP 层面都合法）。

**教训**：加字段必须同时改两处。或者干脆合并成一个结构体（借用型 vs 拥有型，要动序列化路径）。

### 2. 「启动太早」导致静默失效

`device::install()` 由 `quad_main` 调用，而 `quad_main` 在 `Window::from_config()` **之前**执行。

`CMMotionManager.startDeviceMotionUpdates` **依赖 run loop**——那时 Window 没建、游戏循环没跑，run loop 是死的 → 更新根本不发生 → `deviceMotion()` 恒为 None。

API 签名全对、feature 全在、`cargo check` 通过、运行时不报错，**只是永远没数据**。

**教训**：涉及系统服务的初始化，要确认「宿主环境是否已就绪」，不能只看编译通过。

### 3. 「改了表现却以为是修 bug」

iPad 是 4:3，谱面是 16:9。原代码用 `min(aspect_ratio, screen_ar)` 把谱面**横向拉伸**填满 4:3。

我改竖屏时判断「屏幕比谱面窄就锁比例」，结果 4:3 也满足条件 → **横屏被加了上下黑边**，原本全屏的谱面压成中间一条。

**教训**：改「表现」之前先确认它是不是「约定」。4:3 上的横向变形不是 bug，是一贯如此，玩家按那个比例练出了手感。

### 4. 「替换字符串没匹配也不报错」

bump 版本号时写的是 `version = "0.8.2-beta.22"`，但文件里实际是 `beta.20`。`str.replace` 一次没匹配上，**也没报错**，于是 `beta.22`、`beta.23` 两次构建打出来的都是 `beta.20` 的壳。

**教训**：bump 完必须回读确认。`replace` 不报错不等于改到了。

### 5. 「音频异常如何变成时间倒流」

`TimeManager::update` 的自动对齐没有限幅：

```rust
self.start_time -= (music_time - self.now()) * self.force;
```

`force` 只有 `3e-3`，看着温和，但**方向没有约束**。音频一中断，`music.position()` 返回负数 → `start_time` 被推正向 → `now()` 开始递减 → **倒计时往上数 3,4,5 + 谱面倒着走**。

**修法**：`|diff| >= 0.5` 就跳过。**宁可音画不同步，也不让时间倒流**——不同步听得出来、可恢复；时间倒流会让整个判定系统进入不可恢复的错乱。

### 6. 「Neon 免费档的休眠是产品行为，不是配置」

空闲 5 分钟缩到零，免费档**不能关**。保活能缓解，异步队列才是根治。

---

## 六、怎么接手

### 环境

```bash
# 客户端
cd phira-huamei
cargo check -p prpr        # Rust 核心
# iOS 构建走 GitHub Actions（ios-ipa.yml），不改本地 Xcode

# 服务端
cd phm-web
npm run check              # 41 项闸门，与 CI 同款
node tools/hm-stats.mjs    # 数据体检
```

### 发布客户端

```bash
# 1. 改版本（三处必须一致）
#    Cargo.toml                  version
#    .github/workflows/ios-ipa.yml  PHM_APP_VERSION
#    phira/src/client.rs         HUAMEI_CLIENT_VER
# 2. ⚠ 回读确认（吃过亏）
grep -n '^version = "0.8.2' Cargo.toml
grep -n "PHM_APP_VERSION:" .github/workflows/ios-ipa.yml

# 3. 提交并推送
git add -A && git commit -F msg.txt
git push origin HEAD:main

# 4. 触发 iOS 构建（约 7 分钟）
gh workflow run ios-ipa.yml --ref main -R WebExperiments-glitch/phira-huamei
```

### 发布服务端

```bash
# 走 Sites 部署（用户已授权「发完 GitHub 自动推云端」）
# 部署后必须实测线上，不是只看返回成功
```

### 数据库

**Neon 免费档**，连接串在 `phm-web/.env` 的 `PHM_NEON_URL`。

migration 一律新文件：

```bash
node tools/db-migrate.mjs db/migrations/xxx.sql
```

⚠ **`hm_put_play` 已经改过 4 次**（device / max_touch_points / touch_heat / dedup），每次都是新文件全量 `create or replace`——**旧文件已执行，改它不会重跑**。

---

## 七、交接边界（**哪些不该动**）

| 别动 | 为什么 |
|---|---|
| `phm-web/lib/hm.mjs` 的 `validatePlay` 校验口径 | 改了口径历史数据就不可比 |
| `judge_ver` 的语义 | 版本号变了要区分数据来源 |
| 数据库已有的 migration 文件 | 已执行，改了不会重跑 |
| `chart_digest` 的算法 | 变了之后新旧数据无法对齐 |

---

## 八、这件事的价值在哪（留给接手人判断）

**数据飞轮的三个层次**：

```
第一层  谱面特征 → 预测人类表现     ← 现在的能力（ρ 0.816）
第二层  人类表现 → 难度             ← 缺这一层
第三层  人类偏好 → 难度校准         ← 有 692 张争议谱可用
```

**现在的做法是「特征 → 难度」，跳过了一个本该存在的中间量。**

人类表现是**可观测的**（逐音符判定就在库里），难度是**主观的**（要靠投票）。把可观测的当中间量、把主观的放最后一层用 Bradley-Terry 校准——这比直接回归难度合理得多。

**但这一切的前提是数据量**。现在库里只有 16 条，什么结论都做不了。

**所以当前阶段唯一重要的事：攒量。**

---

## 九、联系方式

- 交接人：乌梅干
- 上游：`github.com/TeamFlos/phira`（PR #928 待审）
- 站点：`phm.app.workbuddy.host`
- 数据体检：`node phm-web/tools/hm-stats.mjs`

---

<sub>本文档由 DeepSeek V4.1 Flash 1017 整理于 2026-11-15。</sub>
