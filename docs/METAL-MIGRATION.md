# Metal 后端迁移评估（暂缓）

> 状态：**评估完成，暂不实施**（2026-11-16）
> 结论：技术上值得做，但不为 MetalFX 做；等有量化数据再启动。

## 为什么评估过

用户希望接入 MetalFX（Apple 的超分/时间抗锯齿）。调研后的硬结论：

**MetalFX 在当前的 OpenGL ES 后端上无法使用。**

- MetalFX 全套 API 是 Metal 对象：`MTLFXSpatialScalerDescriptor` →
  `makeSpatialScaler(device: MTLDevice)` → 喂 `MTLTexture`
- 我们的 iOS 后端是 **OpenGL ES 3.0**：
  `vendor/prpr-miniquad/src/native/ios.rs` 里 `EAGLContext initWithAPI:3`
- GLES 纹理不是 `MTLTexture`，MetalFX 无从消费
- iOS 没有 macOS 的 `texImageIOSurface2D`，GL 与 Metal 之间没有官方纹理共享通道

所以「用 MetalFX」等价于「把 Apple 后端换成 Metal」。

## 换 Metal 的真实收益（按对我们项目的重要性排序）

| 收益 | 说明 | 对我们的价值 |
|---|---|---|
| **Compute Shader** | GLES 3.0 不支持（3.1 才有，iOS 不提供） | ⭐⭐⭐ 卡住了真正的 TAA resolve（3×3 邻域钳制）、FSR 风格 EASU 上采样、GPU 端谱面分析 |
| **memoryless Render Target** | Apple GPU 是 TBDR，MSAA resolve 在片上 tile memory 完成，Metal 可声明完全不占显存带宽 | ⭐⭐⭐ iPad 上 MSAA 4x 的带宽/耗电问题 |
| **GPU 调试工具** | Xcode Metal Debugger + GPU Capture，能看每个 pass 的耗时与带宽 | ⭐⭐⭐ 现在查耗电只能靠自建的 `perf: p50/p95/p99` 日志推测 |
| **MetalFX** | 空间/时间上采样 | ⭐⭐ 我们已有 bicubic 上采样，差距有限 |
| **CPU 提交开销** | Metal 预校验 pipeline、多线程编码 | ⭐ 我们 draw call 少，瓶颈在填充率不在 CPU |

**关键洞察**：GLES 在 iOS 上本来就在 Metal 之上（Apple 驱动内部转译）。
性能差距是「多一层转译」，不是硬件能力差异。简单 2D 应用两者帧率常常接近。

## 迁移成本

这不是换开关，是重写渲染层：

1. 重写 `vendor/prpr-miniquad/src/native/apple/` 整个后端（GL → Metal command buffer）
2. 所有 shader 翻译成 MSL：
   - `prpr/src/particle.rs`（粒子 + 后处理）
   - `prpr/src/core/effect.rs`（谱面特效 shader）
   - `prpr/src/core/video.rs`
   - `prpr/src/ui/shadow.rs`
   - `prpr/src/upscale.rs`（我们新加的上采样）
3. macroquad 的渲染抽象要适配 Metal 的 resource binding / argument buffer
4. 双后端维护：Windows 仍是 GL，两套要一起跑 CI

保守估计：**数周量级的工程**，且中途会有大量画面回归。

## 为什么不现在开长期分支

开一个长期不合并的分支，会和主干快速漂移：
- 我们近期还在改设置页、判定条、上报字段
- Metal 分支要反复 rebase，冲突成本随时间指数上升
- 真正动手时，正确的做法是「一次性重写 Apple 后端」，而不是长期并行维护

**决定**：记录评估结论，不进分支。启动条件见下。

## 启动条件（满足任一即可重新评估）

1. **Apple 宣布移除 GLES**（或 beta 版里已不可用）→ 被动但必须
2. **实测数据证明 MSAA/带宽是耗电主因** → 用 `perf` 日志做 MSAA 开/关对比
3. **需要 compute shader 的功能**（真 TAA、GPU 端谱面分析）变成产品刚需

## 在此之前的替代方案（已实施）

- **渲染精度缩放**（100/85/70/60%）：离屏目标按比例缩小，直接省填充率
- **bicubic 上采样**（`prpr/src/upscale.rs`）：替代双线性放大，比 MetalFX Spatial 弱但不依赖 Metal
- **MSAA 2x/4x + 自动降级**：FBO 不完整时回落 1x
- **TAA（实验）**：两帧混合，非完整实现（缺 compute 做邻域钳制）
- **`perf: p50/p95/p99` 日志**：替代 GPU Capture 的初步手段

## 监控项

在 CI 里加一条 GLES 可用性探测：Apple 一旦在 SDK 里移除 `EAGLContext`，
第一时间报警，而不是等发版失败。
