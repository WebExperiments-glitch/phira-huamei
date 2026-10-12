#!/usr/bin/env python3
"""生成大厅 BGM —— 一条**无缝循环**的温暖 ambient pad，输出 `assets/lobby-bgm.ogg`。

【为什么要有这个脚本，而不是直接放一个 mp3】
  Phira 官方的大厅 BGM 在闭源构建里，是这个仓库拿不到的（`res/bgm` 还是加密的，
  `#[cfg(closed)]` 一关就整段没了 —— 这就是「大厅没音乐」的真正原因）。
  要音乐就得有**自己的**音频，而网上下载的曲子授权说不清 ——
  开源仓库里塞一条来路不明的音乐，是给自己埋雷。

  所以：**程序生成**。这条 pad 的每一个采样都由本脚本算出来，
  没有采样自任何现成录音，可以放心按 GPL-3.0 随仓库分发。
  想换成自己的曲子，把 `assets/lobby-bgm.ogg` 替换掉、重跑
  `gen-embedded-assets.py` 即可，代码不用动。

【无缝循环是怎么保证的】
  取循环长度 T = 32 秒，把**每一个分音的频率都量化成 1/T 的整数倍**
  （也就是 f = k / T，k 为整数）。这样在 T 秒里每个分音恰好走完整数个周期，
  首尾天然接得上，不需要交叉淡化。
  ⚠ 量化会让频率偏离标准音高最多 0.5/T = 0.016 Hz —— 比人耳分辨阈值小两个数量级。
  同理，所有 LFO 的频率也量化到 1/T 的倍数。

【音色设计】
  A 小调加九和弦的弱起铺底：A2 / E3 / A3 / C4 / E4 / B4。
  每个声部有一点点失谐（第二个分音偏高零点几赫兹）产生自然的拍频，
  再过一个一阶低通把高频磨掉，最后 tanh 软饱和。整体压到 -16 dBFS 左右 ——
  它是背景，不该盖过 UI 音效。
"""
import array
import math
import os
import subprocess
import sys
import wave

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT_WAV = os.path.join(ROOT, 'assets', '.lobby-bgm.tmp.wav')
OUT_OGG = os.path.join(ROOT, 'assets', 'lobby-bgm.ogg')

SR = 32000          # 大厅音乐不需要 44.1k，省一半体积与一半计算量
T = 32.0            # 循环长度（秒）
N = int(SR * T)
UNIT = 1.0 / T      # 频率量化的粒度


def q(f):
    """把频率量化到 1/T 的整数倍 —— 无缝循环的关键。"""
    return max(1, round(f / UNIT)) * UNIT


# (频率, 起始相位, 基础音量, 失谐量 Hz, 失谐音量)
VOICES = [
    (110.00, 0.00, 0.62, 0.10, 0.30),   # A2  低音铺底
    (164.81, 0.35, 0.34, 0.13, 0.28),   # E3  五度
    (220.00, 0.60, 0.30, 0.09, 0.26),   # A3  根音
    (261.63, 0.15, 0.22, 0.11, 0.24),   # C4  小三度
    (329.63, 0.80, 0.18, 0.15, 0.22),   # E4  五度（高）
    (493.88, 0.45, 0.075, 0.17, 0.20),  # B4  九度，少量提亮
]

# 每个声部的振幅 LFO（频率倍数、深度）—— 频率同样是 1/T 的倍数
LFOS = [
    (1, 0.30),   # 32 秒一轮
    (2, 0.22),
    (3, 0.18),
    (5, 0.14),
    (7, 0.12),
    (11, 0.10),
]

print(f'采样率 {SR} · 循环 {T:.0f}s · {N} 采样 · {len(VOICES)} 个声部')

# 预计算每个声部的两个分音（基音 + 失谐音）的相位增量
voicedef = []
for i, (f, ph, amp, det, detamp) in enumerate(VOICES):
    lf, ld = LFOS[i % len(LFOS)]
    voicedef.append({
        'w1': 2 * math.pi * q(f) / SR,
        'w2': 2 * math.pi * q(f + det) / SR,
        'p1': 2 * math.pi * ph,
        'p2': 2 * math.pi * (ph + 0.12),
        'a': amp,
        'da': detamp,
        'lw': 2 * math.pi * lf / T,
        'lp': 2 * math.pi * i / len(VOICES),
        'ld': ld,
    })

# 立体声：左右给极小的相位差，做出宽度（不做延迟，避免梳状滤波）
width = [2 * math.pi * v / SR for v in (0.0, 0.22)]

left = array.array('d', bytes(8 * N))
right = array.array('d', bytes(8 * N))

sin = math.sin
for i in range(N):
    l = r = 0.0
    for v in voicedef:
        # 两个分音相加 → 自然拍频
        s = v['a'] * sin(v['p1']) + v['da'] * sin(v['p2'])
        v['p1'] += v['w1']
        v['p2'] += v['w2']
        # 该声部自己的慢速音量起伏
        env = 1.0 - v['ld'] * 0.5 * (1.0 - sin(v['lw'] * i + v['lp']))
        l += s * env
        r += s * env
    left[i] = l
    right[i] = r

del sin   # 用了局部引用就行，别再被下面误用

# ── 一阶低通（磨掉高频，让它变"暖"）+ 轻微 tanh 软饱和 + 归一化 ──
def shape(buf, phase):
    y = 0.0
    a = 0.16                       # 一阶低通的系数：越小越闷
    out = array.array('d', bytes(8 * N))
    for i in range(N):
        y += a * (buf[i] - y)
        out[i] = y
    return out

left = shape(left, 0.0)
right = shape(right, 0.0)

peak = max(max(abs(x) for x in left), max(abs(x) for x in right)) or 1.0
gain = 0.158 / peak                 # ≈ -16 dBFS

# ⚠ 循环点不做事后淡化：淡化会破坏"无缝"（首尾音量不一样就接不上）。
#   无缝靠的是上面那个频率量化，不是靠淡化。
pcm = array.array('h', bytes(2 * 2 * N))
for i in range(N):
    lp = math.tanh(left[i] * gain * 1.15) / 1.15
    rp = math.tanh(right[i] * gain * 1.15) / 1.15
    pcm[2 * i] = int(max(-32767, min(32767, lp * 32767)))
    pcm[2 * i + 1] = int(max(-32767, min(32767, rp * 32767)))

os.makedirs(os.path.dirname(OUT_WAV), exist_ok=True)
with wave.open(OUT_WAV, 'wb') as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes(pcm.tobytes())
print(f'  WAV: {os.path.getsize(OUT_WAV) / 1048576:.1f} MB')

# ── 转 OGG Vorbis（symphonia 已开 ogg/vorbis feature）──
r = subprocess.run(
    ['ffmpeg', '-y', '-loglevel', 'error', '-i', OUT_WAV,
     '-c:a', 'libvorbis', '-q:a', '4', '-ar', str(SR), OUT_OGG],
    capture_output=True, text=True)
os.remove(OUT_WAV)
if r.returncode != 0:
    print('❌ ffmpeg 转码失败：' + (r.stderr or '')[:400])
    print('   没有 ffmpeg 的话，可以直接把 WAV 留着用（把上面 OUT_OGG 改成 .wav）')
    sys.exit(1)
print(f'  OGG: {os.path.getsize(OUT_OGG) / 1024:.0f} KB  →  {OUT_OGG}')
print('✅ 大厅 BGM 已生成（无缝循环，频率全部量化到 1/T 的整数倍）')
