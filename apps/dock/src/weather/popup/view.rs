//! Retained Composition UI. A single material backdrop, static weather art.
use super::{Context, data::Data, layout::Layout, text};
use crate::{
    gfx::{Gfx, Ink, WeatherGlyph},
    scene::Theme,
};
use anyhow::Result;
use windows::{
    UI::{
        Color,
        Composition::{CompositionSurfaceBrush, ContainerVisual, Visual},
    },
    core::Interface,
};
use windows_numerics::{Vector2, Vector3};

pub struct View {
    pub root: ContainerVisual,
    pub focus: Vec<Visual>,
}
impl Drop for View {
    fn drop(&mut self) {
        crate::weather::close_tree(&self.root);
    }
}
struct Painter<'a> {
    gfx: &'a Gfx,
    root: &'a ContainerVisual,
    scale: f32,
    ink: Ink,
    icons: Vec<(WeatherGlyph, CompositionSurfaceBrush)>,
}
impl Painter<'_> {
    fn label(&self, b: crate::gfx::LabelBrush, x: f32, y: f32, opacity: f32) -> Result<()> {
        let sprite = self.gfx.compositor.CreateSpriteVisual()?;
        sprite.SetBrush(&b.brush)?;
        sprite.SetSize(Vector2::new(b.width, b.height))?;
        sprite.SetOffset(Vector3::new(x * self.scale, y * self.scale, 0.0))?;
        sprite.SetOpacity(opacity)?;
        self.root.Children()?.InsertAtTop(&sprite)?;
        Ok(())
    }
    fn text(
        &self,
        text: &str,
        position: (f32, f32),
        width: f32,
        font: f32,
        bold: bool,
        opacity: f32,
    ) -> Result<()> {
        let (x, y) = position;
        let b = self.gfx.text_brush_fitted(
            text,
            self.scale,
            font,
            bold,
            self.ink,
            width * self.scale,
        )?;
        self.label(b, x, y, opacity)
    }
    fn centered(
        &self,
        text: &str,
        position: (f32, f32),
        width: f32,
        font: f32,
        bold: bool,
        opacity: f32,
    ) -> Result<()> {
        let (center, y) = position;
        let b = self.gfx.text_brush_fitted(
            text,
            self.scale,
            font,
            bold,
            self.ink,
            width * self.scale,
        )?;
        let x = center - b.width / self.scale / 2.0;
        self.label(b, x, y, opacity)
    }
    fn fill(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) -> Result<Visual> {
        let c = &self.gfx.compositor;
        let sprite = c.CreateSpriteVisual()?;
        sprite.SetSize(Vector2::new(w * self.scale, h * self.scale))?;
        sprite.SetOffset(Vector3::new(x * self.scale, y * self.scale, 0.0))?;
        sprite.SetBrush(&c.CreateColorBrushWithColor(color)?)?;
        if radius > 0.0 {
            let rect = c.CreateRoundedRectangleGeometry()?;
            rect.SetSize(sprite.Size()?)?;
            rect.SetCornerRadius(Vector2::new(radius * self.scale, radius * self.scale))?;
            sprite.SetClip(&c.CreateGeometricClipWithGeometry(&rect)?)?;
        }
        self.root.Children()?.InsertAtTop(&sprite)?;
        Ok(sprite.cast()?)
    }
    fn icon(&mut self, glyph: WeatherGlyph, x: f32, y: f32, size: f32) -> Result<()> {
        // One 56 px raster per distinct glyph in this popup, shared by all rows.
        let brush = match self.icons.iter().find(|(g, _)| *g == glyph) {
            Some((_, brush)) => brush.clone(),
            None => {
                let brush = self.gfx.weather_icon(glyph, 1.4 * self.scale)?;
                self.icons.push((glyph, brush.clone()));
                brush
            }
        };
        let sprite = self.gfx.compositor.CreateSpriteVisual()?;
        sprite.SetBrush(&brush)?;
        sprite.SetSize(Vector2::new(size * self.scale, size * self.scale))?;
        sprite.SetOffset(Vector3::new(x * self.scale, y * self.scale, 0.0))?;
        self.root.Children()?.InsertAtTop(&sprite)?;
        Ok(())
    }
}
fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

pub fn build(gfx: &Gfx, context: Context, layout: Layout, data: &Data) -> Result<View> {
    let s = layout.scale;
    let theme = Theme::for_popup_look(context.look);
    let root = theme.panel(
        gfx,
        Vector2::new(layout.width * s, layout.height * s),
        20.0 * s,
        s,
    )?;
    let muted = if theme.light {
        rgba(0, 0, 0, 22)
    } else {
        rgba(255, 255, 255, 26)
    };
    let mut p = Painter {
        gfx,
        root: &root,
        scale: s,
        ink: if theme.light { Ink::Dark } else { Ink::Light },
        icons: Vec::with_capacity(8),
    };
    let t = crate::weather::text::text(context.language);
    let words = text::text(context.language);
    p.text(t.title, (20.0, 17.0), 310.0, 14.0, true, 1.0)?;
    p.text(data.location, (20.0, 39.0), 305.0, 11.0, false, 0.65)?;
    p.icon(data.glyph, 20.0, 68.0, 56.0)?;
    p.text(&data.degrees, (84.0, 62.0), 137.0, 42.0, false, 1.0)?;
    p.text(data.condition, (20.0, 128.0), 360.0, 13.0, true, 1.0)?;
    p.text(words.wind, (234.0, 69.0), 145.0, 10.0, false, 0.65)?;
    p.text(&data.wind, (234.0, 84.0), 145.0, 13.0, true, 1.0)?;
    p.text(words.feels, (234.0, 106.0), 91.0, 10.0, false, 0.65)?;
    p.text(&data.feels, (328.0, 104.0), 52.0, 13.0, false, 1.0)?;
    if context.has_data {
        p.fill(20.0, 156.0, 360.0, 0.7, 0.0, muted)?;
        p.text(words.hourly, (20.0, 169.0), 245.0, 12.0, true, 0.9)?;
        p.text(
            &format!("{} %", words.rain),
            (290.0, 171.0),
            90.0,
            10.0,
            false,
            0.58,
        )?;
        for (i, hour) in data.hours.iter().enumerate() {
            let x = 20.0 + i as f32 * 60.0;
            if i == 0 {
                p.fill(x, 194.0, 56.0, 96.0, 13.0, muted)?;
            }
            p.centered(&hour.label, (x + 28.0, 201.0), 54.0, 10.0, false, 0.74)?;
            p.icon(hour.glyph, x + 14.0, 219.0, 28.0)?;
            p.centered(&hour.degrees, (x + 28.0, 249.0), 54.0, 14.0, true, 1.0)?;
            p.centered(&hour.rain, (x + 28.0, 271.0), 54.0, 10.0, false, 0.62)?;
        }
        p.fill(20.0, 300.0, 360.0, 0.7, 0.0, muted)?;
        p.text(words.days, (20.0, 313.0), 360.0, 12.0, true, 0.9)?;
        for (i, day) in data.days.iter().enumerate() {
            let y = 338.0 + i as f32 * 30.0;
            p.text(&day.label, (20.0, y + 3.0), 66.0, 12.0, i == 0, 0.92)?;
            p.icon(day.glyph, 90.0, y, 24.0)?;
            p.text(&day.low, (124.0, y + 3.0), 42.0, 12.0, false, 0.55)?;
            p.fill(172.0, y + 11.0, 128.0, 4.0, 2.0, muted)?;
            if let Some((lo, hi)) = data.range {
                let span = (hi - lo).max(1.0);
                let x = (172.0 + (day.range.0 - lo) / span * 128.0).clamp(172.0, 297.0);
                let w = ((day.range.1 - day.range.0) / span * 128.0)
                    .max(3.0)
                    .min(300.0 - x);
                p.fill(x, y + 11.0, w, 4.0, 2.0, rgba(88, 168, 235, 235))?;
            }
            p.text(&day.high, (320.0, y + 3.0), 60.0, 12.0, true, 0.98)?;
        }
        p.fill(20.0, 495.0, 360.0, 0.7, 0.0, muted)?;
        p.text(
            &format!("{}  {}", words.sunrise, data.sunrise),
            (20.0, 504.0),
            176.0,
            10.0,
            false,
            0.72,
        )?;
        p.text(
            &format!("{}  {}", words.sunset, data.sunset),
            (214.0, 504.0),
            166.0,
            10.0,
            false,
            0.72,
        )?;
    } else {
        p.text(data.status, (20.0, 155.0), 360.0, 11.0, false, 0.65)?;
    }
    let mut focus = Vec::with_capacity(3);
    for (index, r) in layout.controls.into_iter().enumerate() {
        let radius = if index == 0 { 14.0 } else { 8.0 };
        let quiet = Color {
            A: muted.A / 2,
            ..muted
        };
        p.fill(r.x, r.y, r.width, r.height, radius, quiet)?;
        let visual = p.fill(r.x, r.y, r.width, r.height, radius, muted)?;
        visual.SetIsVisible(false)?;
        focus.push(visual);
    }
    p.centered("×", (366.0, 17.0), 26.0, 19.0, false, 0.72)?;
    p.text(
        t.settings,
        (28.0, layout.height - 33.0),
        190.0,
        10.5,
        false,
        0.72,
    )?;
    p.text(
        "Open-Meteo ↗",
        (254.0, layout.height - 33.0),
        118.0,
        10.5,
        false,
        0.72,
    )?;
    // Cached data is labelled in the header, without adding another row/clock.
    if !data.status.is_empty() && context.has_data {
        p.text(data.status, (190.0, 40.0), 150.0, 10.0, false, 0.68)?;
    }
    drop(p);
    Ok(View { root, focus })
}
