# Phira · huamei 分支

> **这是 [TeamFlos/phira](https://github.com/TeamFlos/phira) 的一个分支**，
> 不是上游官方仓库。改动集中在「huamei 数据飞轮」：边打谱边把**逐音符判定**
> 上报到 [P.H.M. 谱面定数参考](https://phm.app.workbuddy.host)。
>
> 上游代码为 GPL-3.0-only，本分支同样以 GPL-3.0-only 发布。

## 与上游的差别

| | 上游 | 本分支 |
|---|---|---|
| 在线谱面 | 要登录 Phira 账号 | **完全不需要登录**（登录门槛整段删掉） |
| 资源 | 从 `assets/` 目录读 | **`include_bytes!` 内嵌**，单文件 exe 可直接运行 |
| 成绩上报 | 上报到 Phira 官方 | 上报到本站 `/api/hm/play`，带**逐音符判定**（含每个音符的早/晚毫秒） |
| 账号 | Phira 账号 | 独立的 huamei 账号（本站邮箱 + 验证码），与 Phira 账号无关 |
| `video` feature | 默认开 | 构建时 `--no-default-features` 关掉（省一次 FFmpeg 静态库下载） |
| bundle id | `org.flos.phira` | `app.huamei.phira`（**故意不同**：同一个 ID 会被 iOS 当成同一个应用，侧载会顶掉你装的正版 Phira） |

## 构建

### Windows（本分支的主要目标）

```bash
cargo +nightly-2026-01-01 build --release --no-default-features --bin phira-main
```

产物 `target/release/phira-main.exe` 是**单文件**的，不需要 `assets/` 目录。

⚠ 前提：`assets/` 里的 77 个文件必须在位（`include_bytes!` 是编译期依赖）。
改完 `.gitignore` 或动过 `assets/` 之后跑一遍：

```bash
python huamei-tools/check-embedded-assets.py
```

它会指出哪些文件被 gitignore 挡住（新克隆 / CI 会编译失败）或磁盘上缺失。
上游把 `assets/font.ttf`、`assets/background.jpg` 排掉了，因为上游是**运行时读目录**；
我们改成内嵌之后这两者就是硬依赖，不能再排。

### iOS（无签名 IPA，供 Sideloadly 侧载）

**本地没有 macOS 也能出包** —— 用 GitHub Actions：

```bash
# 手动触发（仓库页 Actions → iOS IPA → Run workflow）
gh workflow run ios-ipa.yml

# 或者打 tag，出包并自动挂到 Release 上
git tag v0.9.6.0 && git push origin v0.9.6.0
```

产物是 `Phira-huamei-unsigned.ipa`（**无签名**）+ `build-report.txt`。
拿到 IPA 后用 Sideloadly 用自己的 Apple ID 签名安装（7 天有效，到期重签）。

**为什么能这么干**：miniquad 的 iOS 后端自己在 Rust 里调
`UIApplicationMain(argc, argv, nil, NSAppDelegate)`，连那个 ObjC 委托类都是
用 objc runtime 现场注册的（见 `src/native/ios.rs`）。所以整个 app 的入口
就是 `phira-main` 这个 Rust 二进制本身，**不需要任何 Swift/ObjC 源文件** ——
`phira.xcodeproj` 的 Sources 阶段确实是空的。缺的只是 `phira.app/Info.plist`
这个外壳，上游把它 gitignore 了，本分支补上并提交。

想本地构建（有 macOS 的话）：

```bash
rustup target add aarch64-apple-ios
xcodebuild -project phira.xcodeproj -target phira -configuration Release \
  -sdk iphoneos CODE_SIGNING_ALLOWED=NO CODE_SIGN_IDENTITY="" \
  DEVELOPMENT_TEAM="" build
```

## 数据上报契约

字段含义、编码格式、服务端校验规则全部写在
[`docs/规范/数据飞轮契约.md`](../phm-web/docs/规范/数据飞轮契约.md)。
**改协议先改文档。**

关键几点：

- 逐音符判定是 **8 字节定长记录**（`prpr/src/huamei.rs`）：`line_id` / `note_id` / `offset_ms` / 判定码 / flags
- `offset_ms = i16::MIN`（−32768）表示**上游没记真实偏移**，不是 0ms。三个来源：
  Miss 的 `0.25` 哨兵、Drag/Flick 硬编码的 `0.`、Hold 按下事件没有偏移
- Hold 的「按下」是 `Err(bool)` 而不是判定，**统计时必须排除** ——
  当成失误会让每个长条都污染数据
- 服务端对 `score`/`accuracy` 不一致**只告警不拒收**：
  拒收真实成绩比放行一条可疑数据糟得多

## 工具

| 脚本 | 作用 |
|---|---|
| `huamei-tools/gen-embedded-assets.py` | 生成 `phira/src/assets_embedded.rs`（77 个 `include_bytes!`） |
| `huamei-tools/prepare-assets.py` | 下载 Noto Sans SC（OFL，可再分发）并生成 `background.jpg` |
| `huamei-tools/check-embedded-assets.py` | 核对 `assets/` 与内嵌清单一致（CI 第一步就跑它） |
| `huamei-tools/patch-pbx-no-default-features.py` | 给 Xcode 的 cargo 脚本加 `--no-default-features` |
| `huamei-tools/patch-pbx-bundle-id.py` | 把 bundle id 改成 `app.huamei.phira` |
