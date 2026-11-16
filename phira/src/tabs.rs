use crate::{anim::Anim, get_data, Result};
use macroquad::prelude::*;
use prpr::{
    ext::{semi_black, RectExt},
    ui::{button_hit, rounded_rect_shadow, RectButton, ShadowConfig, Ui},
};
use std::borrow::Cow;

pub type TitleFn = fn() -> Cow<'static, str>;

struct TabItem<T> {
    value: T,
    title: TitleFn,
    btn: RectButton,
}

pub struct Tabs<T> {
    items: Vec<TabItem<T>>,
    selected: usize,

    y_upper: Anim<f32>,
    y_lower: Anim<f32>,

    content_progress: Anim<f32>,
    prev_go_up: bool,
    prev: usize,

    changed: bool,
}

impl<T> Tabs<T> {
    const LEFT: f32 = -0.94;
    const WIDTH: f32 = 0.2;
    const DURATIONS: (f32, f32) = (0.24, 0.35);
    const CONTENT_DY: f32 = 0.06;
    const CONTENT_DURATION: f32 = 0.4;

    pub fn new(items: impl IntoIterator<Item = (T, TitleFn)>) -> Self {
        Tabs {
            items: items
                .into_iter()
                .map(|(value, title)| TabItem {
                    value,
                    title,
                    btn: RectButton::new(),
                })
                .collect(),
            selected: 0,

            y_upper: Anim::new(0.),
            y_lower: Anim::new(0.),

            content_progress: Anim::new(1.),
            prev_go_up: false,
            prev: 0,

            changed: false,
        }
    }

    /// 每个标签的垂直间距。
    ///
    /// 【话梅】原来写死 0.125。加到 9 个分组后，横屏（ui.top≈0.625，
    /// 可用高度 1.25）从 -top+0.16 开始排，9×0.125=1.125 会直接冲出屏幕底部
    /// ——「关于」被切掉，且没法滚（Tabs 本来就没有滚动）。
    ///
    /// 改成按可用高度反推：够就保持 0.125 的原观感，不够就等比压。
    /// 下限 0.072 是实测还能看清字、点得中的值；真到下限还装不下，
    /// 才需要引入滚动（目前 9 项在 16:10 横屏下是 0.112，够用）。
    fn item_height(&self, ui: &Ui) -> f32 {
        let n = self.items.len().max(1) as f32;
        // 上下各留一点余量，避免贴边
        let avail = (ui.top * 2. - 0.16 - 0.08).max(0.);
        (avail / n).clamp(0.072, 0.125)
    }

    pub fn selected(&self) -> &T {
        &self.items[self.selected].value
    }

    pub fn selected_mut(&mut self) -> &mut T {
        &mut self.items[self.selected].value
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items.iter_mut().map(|item| &mut item.value)
    }

    pub fn changed(&mut self) -> bool {
        let changed = self.changed;
        self.changed = false;
        changed
    }

    pub fn goto(&mut self, t: f32, index: usize) {
        if index == self.selected {
            return;
        }

        let (mut upper, mut lower) = if get_data().prefer_reduced_motion { (0., 0.) } else { Self::DURATIONS };
        if index > self.selected {
            std::mem::swap(&mut upper, &mut lower);
            self.prev_go_up = true;
        } else {
            self.prev_go_up = false;
        }

        self.prev = self.selected;
        self.selected = index;
        self.y_upper.begin(t, upper);
        self.y_lower.begin(t, lower);
        self.content_progress.start(0., 1., t, Self::CONTENT_DURATION);

        self.changed = true;
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> bool {
        for (index, item) in self.items.iter_mut().enumerate() {
            if item.btn.touch(touch) {
                button_hit();
                self.goto(t, index);
                return true;
            }
        }

        false
    }

    fn render_plain(&mut self, ui: &mut Ui, c: Color, first: bool) {
        let h = self.item_height(ui);
        /* 垂直居中：整列关于 y=0 对称。
           ⚠ 这里踩过坑：写成 `(-ui.top*2 + total)/2` 只是把「起点下移」，
             并没有居中 —— total=1.068 时会排到 +0.948，超出 +0.654 的底边，
             「调试/关于」照样看不见。正确的是 `-total/2`。 */
        let total = h * self.items.len() as f32;
        let start = -total / 2.;
        let mut r = Rect::new(Self::LEFT, start, Self::WIDTH, h);
        for (index, item) in self.items.iter_mut().enumerate() {
            if index == self.selected {
                self.y_upper.alter_to(r.y);
                self.y_lower.alter_to(r.bottom());
            }
            item.btn.set(ui, r);
            if first {
                ui.fill_rect(r, semi_black(0.4 * c.a));
            }
            ui.text((item.title)())
                .pos(r.center().x, r.center().y)
                .anchor(0.5, 0.5)
                .no_baseline()
                .size(0.5)
                .color(c)
                .draw();
            r.y += h;
        }
    }

    pub fn render(&mut self, ui: &mut Ui, t: f32, cr: Rect, mut f: impl FnMut(&mut Ui, &mut T) -> Result<()>) -> Result<()> {
        self.render_plain(ui, WHITE, true);

        let y_upper = self.y_upper.now(t);
        let y_lower = self.y_lower.now(t);
        let r = Rect::new(Self::LEFT, y_upper, Self::WIDTH, y_lower - y_upper).nonuniform_feather(0.007, -0.012);
        rounded_rect_shadow(
            ui,
            r,
            &ShadowConfig {
                radius: 0.008,
                base: 0.5,
                ..Default::default()
            },
        );
        ui.fill_path(&r.rounded(0.008), WHITE);
        ui.scissor(r, |ui| self.render_plain(ui, BLACK, false));

        ui.fill_path(&cr.rounded(0.005), semi_black(0.4));
        ui.scissor::<Result<()>>(cr, |ui| {
            let p = if get_data().prefer_reduced_motion {
                1.
            } else {
                self.content_progress.now(t)
            };
            if p < 1. {
                ui.scope(|ui| {
                    let dy = Self::CONTENT_DY * p;
                    ui.dy(if self.prev_go_up { -dy } else { dy });
                    ui.alpha(1. - p, |ui| f(ui, &mut self.items[self.prev].value))
                })?;
            }

            ui.scope(|ui| {
                let dy = Self::CONTENT_DY * (1. - p);
                ui.dy(if self.prev_go_up { dy } else { -dy });
                ui.alpha(p, |ui| f(ui, &mut self.items[self.selected].value))
            })?;

            Ok(())
        })?;

        Ok(())
    }
}
