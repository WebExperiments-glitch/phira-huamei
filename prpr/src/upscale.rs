//! 【话梅】高质量空间上采样 —— MetalFX Spatial 在我们这套管线里的等效实现。
//!
//! ## 为什么不是 MetalFX
//!
//! MetalFX 是 **Metal 专属 API**：`MTLFXSpatialScalerDescriptor` →
//! `makeSpatialScaler(device: MTLDevice)` → 喂 `MTLTexture`。
//! 而我们 iOS 的后端是 **OpenGL ES 3.0**（见 vendor/prpr-miniquad/src/native/ios.rs
//! 里的 `EAGLContext initWithAPI:3`），纹理不是 MTLTexture，MetalFX 无从消费；
//! iOS 也没有 macOS 的 `texImageIOSurface2D` 让 GL 与 Metal 共享纹理。
//! 要真用 MetalFX，只能把整个 Apple 后端换成 Metal —— 那是重写渲染层。
//!
//! ## 这里做的是什么
//!
//! MetalFX Spatial 的核心思路是「低分辨率渲染 + 智能放大」，放大质量取决于
//! 采样核。macOS 上默认的双线性放大很糊；换成 **bicubic（Catmull-Rom）**
//! 能明显找回边缘锐度，且没有 MetalFX Temporal 那种需要运动矢量的复杂度。
//!
//! 实现方式是 4-tap 加权混合（每个轴用 Catmull-Rom 权重折叠成两次 lerp），
//! 视觉上接近 16-tap 的双三次采样，但在移动 GPU 上便宜得多。
//!
//! ⚠ 与 MetalFX 的差距：MetalFX 还会做边缘方向检测（EASU），
//!   在斜线/细线上更锐；bicubic 只能保证「比双线性明显好」。

use macroquad::{
    material::{gl_use_default_material, gl_use_material, load_material, Material, MaterialParams},
    miniquad::UniformType,
    prelude::*,
};

const VERTEX: &str = r#"#version 100
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;
varying lowp vec2 uv;
varying lowp vec4 color;
void main() {
    gl_Position = vec4(position, 1.0);
    uv = texcoord;
    color = color0;
}
"#;

const FRAGMENT: &str = r#"#version 100
precision mediump float;

varying lowp vec2 uv;
varying lowp vec4 color;
uniform sampler2D Texture;
// 输入纹理的像素尺寸（不是倒数），用于把采样点换算成纹素
uniform vec2 texSize;

// Catmull-Rom 的 4 个权重。x 是区间内的小数部分。
vec4 cubic(float v) {
    vec4 n = vec4(1.0, 2.0, 3.0, 4.0) - v;
    vec4 s = n * n * n;
    float x = s.x;
    float y = s.y - 4.0 * s.x;
    float z = s.z - 4.0 * s.y + 6.0 * s.x;
    float w = 6.0 - x - y - z;
    return vec4(x, y, z, w) * (1.0 / 6.0);
}

vec4 bicubic(sampler2D tex, vec2 texUV, vec2 size) {
    vec2 coord = texUV * size - 0.5;
    vec2 fxy = fract(coord);
    coord -= fxy;

    vec4 xcubic = cubic(fxy.x);
    vec4 ycubic = cubic(fxy.y);

    vec4 c = coord.xxyy + vec2(-0.5, 1.5).xyxy;
    vec4 s = vec4(xcubic.xz + xcubic.yw, ycubic.xz + ycubic.yw);
    vec4 offset = c + vec4(xcubic.yw, ycubic.yw) / s;
    offset /= size.xxyy;

    vec4 sample0 = texture2D(tex, offset.xz);
    vec4 sample1 = texture2D(tex, offset.yz);
    vec4 sample2 = texture2D(tex, offset.xw);
    vec4 sample3 = texture2D(tex, offset.yw);

    float sx = s.x / (s.x + s.y);
    float sy = s.z / (s.z + s.w);
    return mix(mix(sample3, sample2, sx), mix(sample1, sample0, sx), sy);
}

void main() {
    vec4 c = bicubic(Texture, uv, texSize);
    /* 轻微锐化（unsharp mask 的简版）：把中心与 4 邻域均值之差按小比例叠加。
       音游画面里判定线、音符边缘对比度高，放大后容易发软，这一步能补回来。
       系数压得很低（0.12），避免高对比区域出现白边振铃。 */
    vec2 t = 1.0 / texSize;
    vec3 n = texture2D(Texture, uv + vec2(0.0, -t.y)).rgb;
    vec3 s = texture2D(Texture, uv + vec2(0.0,  t.y)).rgb;
    vec3 w = texture2D(Texture, uv + vec2(-t.x, 0.0)).rgb;
    vec3 e = texture2D(Texture, uv + vec2( t.x, 0.0)).rgb;
    vec3 blur = (n + s + w + e) * 0.25;
    vec3 sharpened = c.rgb + (c.rgb - blur) * 0.12;
    gl_FragColor = vec4(clamp(sharpened, 0.0, 1.0), c.a) * color;
}
"#;

pub struct Upscaler {
    material: Material,
    in_size: (f32, f32),
}

impl Upscaler {
    pub fn new(in_size: (f32, f32)) -> anyhow::Result<Self> {
        let material = load_material(
            VERTEX,
            FRAGMENT,
            MaterialParams {
                uniforms: vec![("texSize".to_owned(), UniformType::Float2)],
                textures: vec![],
                ..Default::default()
            },
        )?;
        Ok(Self { material, in_size })
    }

    /// 输入纹理尺寸变了（改渲染精度 / 转屏）要同步，否则采样偏移算错。
    pub fn set_input_size(&mut self, size: (f32, f32)) {
        if (self.in_size.0 - size.0).abs() > 0.5 || (self.in_size.1 - size.1).abs() > 0.5 {
            self.in_size = size;
        }
    }

    /// 用上采样 shader 画一张全屏纹理。
    ///
    /// 调用方需已设好相机（屏幕坐标系）；这里只负责换 material + 画 quad。
    pub fn draw(&mut self, tex: Texture2D, top: f32) {
        self.material
            .set_uniform("texSize", vec2(self.in_size.0, self.in_size.1));
        gl_use_material(self.material);
        draw_texture_ex(
            tex,
            -1.,
            -top,
            WHITE,
            DrawTextureParams {
                dest_size: Some(vec2(2., top * 2.)),
                ..Default::default()
            },
        );
        gl_use_default_material();
    }
}

impl Drop for Upscaler {
    fn drop(&mut self) {
        self.material.delete();
    }
}
