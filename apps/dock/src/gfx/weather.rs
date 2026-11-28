//! Weather illustrations, rendered once per glyph/DPI rebuild: a glowing sun,
//! soft clouds, a crescent moon with stars, rain, snow, lightning and fog,
//! as the macOS and iOS Weather apps draw them. All in a 40 x 40 box.
use super::*;
use windows::Win32::Graphics::Direct2D::{
    D2D1_CAP_STYLE_ROUND, D2D1_COMBINE_MODE, D2D1_COMBINE_MODE_EXCLUDE, D2D1_DASH_STYLE_SOLID,
    D2D1_LINE_JOIN_ROUND, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_STROKE_STYLE_PROPERTIES,
    ID2D1Brush, ID2D1Factory, ID2D1Geometry, ID2D1StrokeStyle,
};

impl Gfx {
    pub fn weather_icon(&self, glyph: WeatherGlyph, scale: f32) -> Result<CompositionSurfaceBrush> {
        let side = (40.0 * scale).round().max(1.0) as u32;
        self.draw(side, side, |dc| paint(dc, glyph, scale))
    }
}

/// Colors as the Weather apps use them.
mod palette {
    use super::{D2D1_COLOR_F, rgb};
    pub const SUN_CORE: D2D1_COLOR_F = rgb(1.0, 0.97, 0.62, 1.0);
    pub const SUN_MID: D2D1_COLOR_F = rgb(1.0, 0.82, 0.16, 1.0);
    pub const SUN_EDGE: D2D1_COLOR_F = rgb(1.0, 0.56, 0.0, 1.0);
    pub const SUN_RAY: D2D1_COLOR_F = rgb(1.0, 0.76, 0.1, 1.0);
    pub const SUN_GLOW: D2D1_COLOR_F = rgb(1.0, 0.84, 0.3, 0.55);
    pub const CLOUD_TOP: D2D1_COLOR_F = rgb(1.0, 1.0, 1.0, 1.0);
    pub const CLOUD_BOTTOM: D2D1_COLOR_F = rgb(0.80, 0.87, 0.95, 1.0);
    pub const BACK_TOP: D2D1_COLOR_F = rgb(0.90, 0.93, 0.97, 1.0);
    pub const BACK_BOTTOM: D2D1_COLOR_F = rgb(0.66, 0.73, 0.83, 1.0);
    pub const DARK_TOP: D2D1_COLOR_F = rgb(0.80, 0.84, 0.91, 1.0);
    pub const DARK_BOTTOM: D2D1_COLOR_F = rgb(0.46, 0.52, 0.63, 1.0);
    pub const SHADOW: D2D1_COLOR_F = rgb(0.05, 0.12, 0.28, 0.14);
    pub const MOON_LIGHT: D2D1_COLOR_F = rgb(1.0, 0.99, 0.90, 1.0);
    pub const MOON_SHADE: D2D1_COLOR_F = rgb(0.95, 0.86, 0.58, 1.0);
    pub const MOON_GLOW: D2D1_COLOR_F = rgb(0.98, 0.94, 0.75, 0.40);
    pub const STAR: D2D1_COLOR_F = rgb(1.0, 0.98, 0.86, 1.0);
    pub const RAIN_TOP: D2D1_COLOR_F = rgb(0.80, 0.96, 1.0, 1.0);
    pub const RAIN_BOTTOM: D2D1_COLOR_F = rgb(0.36, 0.78, 1.0, 1.0);
    pub const SNOW: D2D1_COLOR_F = rgb(0.96, 0.99, 1.0, 1.0);
    pub const BOLT_TOP: D2D1_COLOR_F = rgb(1.0, 0.95, 0.45, 1.0);
    pub const BOLT_BOTTOM: D2D1_COLOR_F = rgb(1.0, 0.62, 0.0, 1.0);
    pub const FOG: D2D1_COLOR_F = rgb(0.86, 0.90, 0.96, 0.95);
}

/// How a cloud is shaded.
#[derive(Clone, Copy)]
enum Shade {
    /// White, in front.
    Front,
    /// Grayer, behind another cloud.
    Back,
    /// Heavy, for rain, snow and storms.
    Dark,
}

/// The drawing tools for one icon: the device context, its factory, the
/// scale and round line ends.
struct Pen<'a> {
    dc: &'a ID2D1DeviceContext,
    factory: ID2D1Factory,
    scale: f32,
    round: ID2D1StrokeStyle,
}

impl Pen<'_> {
    fn point(&self, x: f32, y: f32) -> Vector2 {
        Vector2::new(x * self.scale, y * self.scale)
    }

    fn circle(&self, x: f32, y: f32, r: f32) -> D2D1_ELLIPSE {
        D2D1_ELLIPSE {
            point: self.point(x, y),
            radiusX: r * self.scale,
            radiusY: r * self.scale,
        }
    }

    fn solid(&self, color: D2D1_COLOR_F) -> Result<ID2D1Brush> {
        // SAFETY: A brush on the live drawing context.
        Ok(unsafe { self.dc.CreateSolidColorBrush(&color, None)? }.cast()?)
    }

    fn stops(
        &self,
        stops: &[(f32, D2D1_COLOR_F)],
    ) -> Result<windows::Win32::Graphics::Direct2D::ID2D1GradientStopCollection> {
        let stops: Vec<_> = stops
            .iter()
            .map(|&(position, color)| D2D1_GRADIENT_STOP { position, color })
            .collect();
        // SAFETY: The stops live for the call.
        Ok(unsafe {
            self.dc.CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?
        }
        .cast()?)
    }

    /// From `top` at `y0` to `bottom` at `y1`.
    fn vertical(
        &self,
        top: D2D1_COLOR_F,
        bottom: D2D1_COLOR_F,
        y0: f32,
        y1: f32,
    ) -> Result<ID2D1Brush> {
        let stops = self.stops(&[(0.0, top), (1.0, bottom)])?;
        // SAFETY: The stops live for the call.
        Ok(unsafe {
            self.dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: self.point(20.0, y0),
                    endPoint: self.point(20.0, y1),
                },
                None,
                &stops,
            )?
        }
        .cast()?)
    }

    /// Around `(x, y)` out to `radius`, lit from `light` (an offset of the
    /// centre) for a rounded look.
    fn radial(
        &self,
        x: f32,
        y: f32,
        radius: f32,
        light: (f32, f32),
        stops: &[(f32, D2D1_COLOR_F)],
    ) -> Result<ID2D1Brush> {
        let stops = self.stops(stops)?;
        // SAFETY: The stops live for the call.
        Ok(unsafe {
            self.dc.CreateRadialGradientBrush(
                &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                    center: self.point(x, y),
                    gradientOriginOffset: Vector2::new(light.0 * self.scale, light.1 * self.scale),
                    radiusX: radius * self.scale,
                    radiusY: radius * self.scale,
                },
                None,
                &stops,
            )?
        }
        .cast()?)
    }

    fn circle_geometry(&self, x: f32, y: f32, r: f32) -> Result<ID2D1Geometry> {
        // SAFETY: A geometry from the context's own factory.
        Ok(unsafe { self.factory.CreateEllipseGeometry(&self.circle(x, y, r))? }.cast()?)
    }

    fn combine(
        &self,
        a: &ID2D1Geometry,
        b: &ID2D1Geometry,
        mode: D2D1_COMBINE_MODE,
    ) -> Result<ID2D1Geometry> {
        // SAFETY: The sink is opened, written by the combination and closed.
        unsafe {
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            a.CombineWithGeometry(b, mode, None, 0.05, &sink)?;
            sink.Close()?;
            Ok(path.cast()?)
        }
    }

    /// A closed shape through `points`.
    fn polygon(&self, points: &[(f32, f32)]) -> Result<ID2D1Geometry> {
        // SAFETY: The sink is opened, given one closed figure and closed.
        unsafe {
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            let (first, rest) = points.split_first().context("empty polygon")?;
            sink.BeginFigure(self.point(first.0, first.1), D2D1_FIGURE_BEGIN_FILLED);
            for &(x, y) in rest {
                sink.AddLine(self.point(x, y));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            Ok(path.cast()?)
        }
    }

    fn fill(&self, geometry: &ID2D1Geometry, brush: &ID2D1Brush) {
        // SAFETY: Geometry and brush live for the call.
        unsafe { self.dc.FillGeometry(geometry, brush, None) };
    }

    fn line(&self, from: (f32, f32), to: (f32, f32), brush: &ID2D1Brush, width: f32) {
        // SAFETY: Brush and stroke style live for the call.
        unsafe {
            self.dc.DrawLine(
                self.point(from.0, from.1),
                self.point(to.0, to.1),
                brush,
                width * self.scale,
                &self.round,
            );
        }
    }

    /// A sun at `(x, y)` with disc radius `r`: glow, rays, then the disc.
    fn sun(&self, x: f32, y: f32, r: f32) -> Result<()> {
        let glow = self.radial(
            x,
            y,
            r * 2.05,
            (0.0, 0.0),
            &[(0.45, palette::SUN_GLOW), (1.0, rgb(1.0, 0.8, 0.25, 0.0))],
        )?;
        // SAFETY: Brush lives for the call.
        unsafe { self.dc.FillEllipse(&self.circle(x, y, r * 2.05), &glow) };
        let rays = self.solid(palette::SUN_RAY)?;
        for ray in 0..8 {
            let angle = ray as f32 * std::f32::consts::FRAC_PI_4 + std::f32::consts::FRAC_PI_8;
            let (cos, sin) = (angle.cos(), angle.sin());
            self.line(
                (x + cos * (r + 2.6), y + sin * (r + 2.6)),
                (x + cos * (r + 5.4), y + sin * (r + 5.4)),
                &rays,
                2.3,
            );
        }
        let disc = self.radial(
            x,
            y,
            r,
            (-r * 0.35, -r * 0.4),
            &[
                (0.0, palette::SUN_CORE),
                (0.55, palette::SUN_MID),
                (1.0, palette::SUN_EDGE),
            ],
        )?;
        // SAFETY: Brush lives for the call.
        unsafe { self.dc.FillEllipse(&self.circle(x, y, r), &disc) };
        Ok(())
    }

    /// A crescent moon around `(x, y)`, radius `r`, with its glow.
    fn moon(&self, x: f32, y: f32, r: f32) -> Result<()> {
        let glow = self.radial(
            x,
            y,
            r * 1.8,
            (0.0, 0.0),
            &[(0.5, palette::MOON_GLOW), (1.0, rgb(0.98, 0.94, 0.75, 0.0))],
        )?;
        // SAFETY: Brush lives for the call.
        unsafe { self.dc.FillEllipse(&self.circle(x, y, r * 1.8), &glow) };
        let crescent = self.combine(
            &self.circle_geometry(x, y, r)?,
            &self.circle_geometry(x + r * 0.62, y - r * 0.38, r * 0.86)?,
            D2D1_COMBINE_MODE_EXCLUDE,
        )?;
        let light = self.radial(
            x - r * 0.3,
            y,
            r * 1.2,
            (-r * 0.2, -r * 0.2),
            &[(0.0, palette::MOON_LIGHT), (1.0, palette::MOON_SHADE)],
        )?;
        self.fill(&crescent, &light);
        Ok(())
    }

    /// A four-pointed sparkle at `(x, y)`.
    fn star(&self, x: f32, y: f32, size: f32) -> Result<()> {
        let (a, b) = (size, size * 0.28);
        let star = self.polygon(&[
            (x, y - a),
            (x + b, y - b),
            (x + a, y),
            (x + b, y + b),
            (x, y + a),
            (x - b, y + b),
            (x - a, y),
            (x - b, y - b),
        ])?;
        self.fill(&star, &self.solid(palette::STAR)?);
        Ok(())
    }

    /// A cloud `width` wide with its top-left at `(x, y)`; it is 0.62 as tall.
    fn cloud(&self, x: f32, y: f32, width: f32, shade: Shade) -> Result<()> {
        let s = self.scale;
        let at = |u: f32, v: f32| (x + u * width, y + v * width);
        let (bx, by) = at(0.0, 0.31);
        let (ex, ey) = at(1.0, 0.62);
        let base = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: bx * s,
                top: by * s,
                right: ex * s,
                bottom: ey * s,
            },
            radiusX: 0.155 * width * s,
            radiusY: 0.155 * width * s,
        };
        let bumps =
            [(0.28, 0.37, 0.21), (0.56, 0.27, 0.27), (0.80, 0.42, 0.17)].map(|(u, v, r)| {
                let (cx, cy) = at(u, v);
                self.circle(cx, cy, r * width)
            });
        // Each piece is filled with the same brush, whose gradient is set in
        // the box, not per piece: opaque, the pieces read as one shape
        // without combining geometries, which cost several times more.
        let pieces = |brush: &ID2D1Brush| {
            // SAFETY: Shapes and brush live for these calls.
            unsafe {
                self.dc.FillRoundedRectangle(&base, brush);
                for bump in &bumps {
                    self.dc.FillEllipse(bump, brush);
                }
            }
        };
        let (top, bottom) = match shade {
            Shade::Front => (palette::CLOUD_TOP, palette::CLOUD_BOTTOM),
            Shade::Back => (palette::BACK_TOP, palette::BACK_BOTTOM),
            Shade::Dark => (palette::DARK_TOP, palette::DARK_BOTTOM),
        };
        // A soft shadow under it: only its rim below the cloud shows, where
        // the pieces do not overlap.
        let shadow = self.solid(palette::SHADOW)?;
        // SAFETY: The transform is restored right after the shadow.
        unsafe {
            let mut transform = Matrix3x2::default();
            self.dc.GetTransform(&mut transform);
            self.dc
                .SetTransform(&(Matrix3x2::translation(0.0, 1.1 * s) * transform));
            self.dc.FillRoundedRectangle(&base, &shadow);
            self.dc.SetTransform(&transform);
        }
        let (_, top_y) = at(0.0, 0.0);
        let (_, bottom_y) = at(0.0, 0.62);
        pieces(&self.vertical(top, bottom, top_y, bottom_y)?);
        Ok(())
    }

    /// Slanted rain drops along `xs`, from `y`.
    fn rain(&self, xs: &[f32], y: f32) -> Result<()> {
        let drops = self.vertical(palette::RAIN_TOP, palette::RAIN_BOTTOM, y, y + 7.0)?;
        for (index, &x) in xs.iter().enumerate() {
            let y = y + if index % 2 == 1 { 2.5 } else { 0.0 };
            self.line((x, y), (x - 2.0, y + 5.5), &drops, 2.7);
        }
        Ok(())
    }

    /// A six-armed snowflake at `(x, y)`.
    fn flake(&self, x: f32, y: f32, r: f32) -> Result<()> {
        let snow = self.solid(palette::SNOW)?;
        for arm in 0..3 {
            let angle = arm as f32 * std::f32::consts::FRAC_PI_3 + std::f32::consts::FRAC_PI_2;
            let (dx, dy) = (angle.cos() * r, angle.sin() * r);
            self.line((x - dx, y - dy), (x + dx, y + dy), &snow, 1.35);
        }
        Ok(())
    }

    /// A lightning bolt from about `(x, y)` down.
    fn bolt(&self, x: f32, y: f32) -> Result<()> {
        let bolt = self.polygon(&[
            (x + 2.6, y),
            (x - 4.2, y + 9.0),
            (x - 0.2, y + 9.0),
            (x - 3.4, y + 16.5),
            (x + 5.4, y + 6.4),
            (x + 1.2, y + 6.4),
            (x + 4.4, y),
        ])?;
        self.fill(
            &bolt,
            &self.vertical(palette::BOLT_TOP, palette::BOLT_BOTTOM, y, y + 16.5)?,
        );
        Ok(())
    }

    /// Rounded fog bands: `(left, right, y)` each.
    fn fog(&self, bands: &[(f32, f32, f32)]) -> Result<()> {
        let fog = self.solid(palette::FOG)?;
        for &(left, right, y) in bands {
            self.line((left, y), (right, y), &fog, 2.6);
        }
        Ok(())
    }
}

/// Draws `glyph` in a 40 x 40 box at `scale`.
fn paint(dc: &ID2D1DeviceContext, glyph: WeatherGlyph, scale: f32) -> Result<()> {
    // SAFETY: Reads the factory of the live context; the stroke style is
    // made from it with valid properties.
    let (factory, round) = unsafe {
        let factory: ID2D1Factory = dc.GetFactory()?;
        let round = factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            },
            None,
        )?;
        (factory, round)
    };
    let pen = Pen {
        dc,
        factory,
        scale,
        round,
    };
    match glyph {
        WeatherGlyph::Sun => pen.sun(20.0, 20.0, 8.6)?,
        WeatherGlyph::Moon => {
            pen.moon(18.5, 21.0, 11.0)?;
            pen.star(30.5, 9.0, 2.6)?;
            pen.star(33.0, 18.5, 1.6)?;
        }
        WeatherGlyph::PartlyCloudy => {
            pen.sun(25.5, 14.0, 7.2)?;
            pen.cloud(3.5, 16.0, 27.5, Shade::Front)?;
        }
        WeatherGlyph::NightCloud => {
            pen.moon(25.0, 13.5, 8.0)?;
            pen.star(35.0, 6.5, 1.7)?;
            pen.cloud(3.5, 16.0, 27.5, Shade::Front)?;
        }
        WeatherGlyph::Cloud => {
            pen.cloud(14.0, 9.0, 22.0, Shade::Back)?;
            pen.cloud(3.0, 15.5, 29.0, Shade::Front)?;
        }
        WeatherGlyph::Rain => {
            pen.cloud(4.5, 5.5, 31.0, Shade::Dark)?;
            pen.rain(&[13.0, 20.5, 28.0], 28.5)?;
        }
        WeatherGlyph::Snow => {
            pen.cloud(4.5, 5.5, 31.0, Shade::Back)?;
            pen.flake(12.5, 31.5, 3.0)?;
            pen.flake(20.5, 35.0, 3.0)?;
            pen.flake(28.5, 31.5, 3.0)?;
        }
        WeatherGlyph::Storm => {
            pen.cloud(4.5, 5.5, 31.0, Shade::Dark)?;
            pen.bolt(19.5, 22.5)?;
        }
        WeatherGlyph::Fog => {
            pen.cloud(6.0, 4.5, 28.0, Shade::Front)?;
            pen.fog(&[(8.0, 32.0, 27.5), (11.0, 35.0, 32.0), (7.5, 27.5, 36.5)])?;
        }
        // No forecast yet: a quiet gray cloud.
        WeatherGlyph::Unknown => pen.cloud(5.0, 11.0, 30.0, Shade::Back)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct2D::{
        D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET,
        D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_MAP_OPTIONS_READ,
    };

    /// Draws every glyph at 3x over the card's blue and writes the pixels
    /// (BGRA, premultiplied) to `MUELLE_WEATHER_PREVIEW` to look at them.
    #[test]
    #[ignore = "writes a preview of the weather icons"]
    fn preview_weather_icons() -> Result<()> {
        let Some(out) = std::env::var_os("MUELLE_WEATHER_PREVIEW") else {
            return Ok(());
        };
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let glyphs = [
            WeatherGlyph::Sun,
            WeatherGlyph::PartlyCloudy,
            WeatherGlyph::Cloud,
            WeatherGlyph::Rain,
            WeatherGlyph::Storm,
            WeatherGlyph::Snow,
            WeatherGlyph::Fog,
            WeatherGlyph::Moon,
            WeatherGlyph::NightCloud,
            WeatherGlyph::Unknown,
        ];
        let scale = 3.0;
        let side = (40.0 * scale) as u32;
        let size = D2D_SIZE_U {
            width: side * glyphs.len() as u32,
            height: side,
        };
        let format = D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        };
        unsafe {
            let dc = gfx
                ._d2d
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let target = dc.CreateBitmap(
                size,
                None,
                0,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: format,
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    ..Default::default()
                },
            )?;
            dc.SetTarget(&target);
            dc.BeginDraw();
            dc.Clear(Some(&rgb(0.20, 0.50, 0.90, 1.0)));
            for (index, glyph) in glyphs.into_iter().enumerate() {
                dc.SetTransform(&Matrix3x2::translation((index as u32 * side) as f32, 0.0));
                paint(&dc, glyph, scale)?;
            }
            dc.EndDraw(None, None)?;
            let read = dc.CreateBitmap(
                size,
                None,
                0,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: format,
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    ..Default::default()
                },
            )?;
            read.CopyFromBitmap(None, &target, None)?;
            let map = read.Map(D2D1_MAP_OPTIONS_READ)?;
            let mut pixels = Vec::with_capacity((size.width * size.height * 4) as usize);
            for row in 0..size.height {
                let line = std::slice::from_raw_parts(
                    map.bits.add((row * map.pitch) as usize),
                    (size.width * 4) as usize,
                );
                pixels.extend_from_slice(line);
            }
            read.Unmap()?;
            std::fs::write(&out, pixels)?;
            println!("{}x{}", size.width, size.height);
        }
        Ok(())
    }
}
