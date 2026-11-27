//! Small static weather illustrations, rendered once per glyph/DPI rebuild.
use super::*;

impl Gfx {
    pub fn weather_icon(&self, glyph: WeatherGlyph, scale: f32) -> Result<CompositionSurfaceBrush> {
        let side = (40.0 * scale).round().max(1.0) as u32;
        self.draw(side, side, |dc| {
            // SAFETY: The callback owns the drawing session; all geometry is
            // bounded to this tiny surface and brushes are local COM owners.
            unsafe {
                let yellow = dc.CreateSolidColorBrush(&rgb(1.0, 0.76, 0.12, 1.0), None)?;
                let white = dc.CreateSolidColorBrush(&rgb(0.91, 0.96, 1.0, 1.0), None)?;
                let gray = dc.CreateSolidColorBrush(&rgb(0.65, 0.76, 0.87, 1.0), None)?;
                let blue = dc.CreateSolidColorBrush(&rgb(0.28, 0.69, 1.0, 1.0), None)?;
                let point = |x, y| Vector2::new(x * scale, y * scale);
                let ellipse = |x, y, rx, ry| D2D1_ELLIPSE {
                    point: point(x, y),
                    radiusX: rx * scale,
                    radiusY: ry * scale,
                };
                let sunny = matches!(glyph, WeatherGlyph::Sun | WeatherGlyph::PartlyCloudy);
                if sunny {
                    let (cx, cy, radius) = if glyph == WeatherGlyph::Sun {
                        (20.0, 20.0, 8.5)
                    } else {
                        (15.0, 13.0, 6.8)
                    };
                    for ray in 0..8 {
                        let a = ray as f32 * std::f32::consts::FRAC_PI_4;
                        dc.DrawLine(
                            point(cx + a.cos() * (radius + 3.0), cy + a.sin() * (radius + 3.0)),
                            point(cx + a.cos() * (radius + 6.5), cy + a.sin() * (radius + 6.5)),
                            &yellow,
                            2.4 * scale,
                            None,
                        );
                    }
                    dc.FillEllipse(&ellipse(cx, cy, radius, radius), &yellow);
                }
                if matches!(glyph, WeatherGlyph::Moon | WeatherGlyph::NightCloud) {
                    let (x, y, r) = if glyph == WeatherGlyph::Moon {
                        (20.0, 19.0, 12.0)
                    } else {
                        (15.0, 13.0, 9.0)
                    };
                    dc.FillEllipse(&ellipse(x, y, r, r), &white);
                    dc.FillEllipse(&ellipse(x - 3.0, y - 3.0, r * 0.2, r * 0.2), &gray);
                    dc.FillEllipse(&ellipse(x + 4.0, y + 4.0, r * 0.25, r * 0.25), &gray);
                }
                if matches!(
                    glyph,
                    WeatherGlyph::Cloud
                        | WeatherGlyph::PartlyCloudy
                        | WeatherGlyph::NightCloud
                        | WeatherGlyph::Rain
                        | WeatherGlyph::Snow
                        | WeatherGlyph::Storm
                        | WeatherGlyph::Fog
                ) {
                    dc.FillEllipse(&ellipse(13.0, 24.0, 9.0, 7.0), &gray);
                    dc.FillEllipse(&ellipse(23.0, 20.0, 10.0, 9.0), &white);
                    dc.FillEllipse(&ellipse(31.0, 25.0, 7.0, 6.0), &white);
                    dc.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: 9.0 * scale,
                                top: 23.0 * scale,
                                right: 33.0 * scale,
                                bottom: 31.0 * scale,
                            },
                            radiusX: 4.0 * scale,
                            radiusY: 4.0 * scale,
                        },
                        &white,
                    );
                }
                if matches!(
                    glyph,
                    WeatherGlyph::Rain | WeatherGlyph::Snow | WeatherGlyph::Storm
                ) {
                    for x in [12.0, 22.0, 31.0] {
                        if glyph == WeatherGlyph::Snow {
                            dc.FillEllipse(&ellipse(x, 36.0, 1.5, 1.5), &white);
                        } else {
                            dc.DrawLine(
                                point(x, 33.0),
                                point(x - 2.0, 38.0),
                                &blue,
                                2.2 * scale,
                                None,
                            );
                        }
                    }
                }
                if glyph == WeatherGlyph::Fog {
                    for (x, y) in [(8.0, 34.0), (14.0, 38.0)] {
                        dc.DrawLine(point(x, y), point(x + 19.0, y), &gray, 2.0 * scale, None);
                    }
                }
                if glyph == WeatherGlyph::Storm {
                    dc.DrawLine(
                        point(25.0, 28.0),
                        point(21.0, 34.0),
                        &yellow,
                        3.0 * scale,
                        None,
                    );
                    dc.DrawLine(
                        point(21.0, 34.0),
                        point(26.0, 33.0),
                        &yellow,
                        3.0 * scale,
                        None,
                    );
                    dc.DrawLine(
                        point(26.0, 33.0),
                        point(22.0, 39.0),
                        &yellow,
                        3.0 * scale,
                        None,
                    );
                }
                if glyph == WeatherGlyph::Unknown {
                    dc.DrawEllipse(&ellipse(20.0, 20.0, 11.0, 11.0), &gray, 2.0 * scale, None);
                    dc.DrawLine(
                        point(15.0, 20.0),
                        point(25.0, 20.0),
                        &gray,
                        2.0 * scale,
                        None,
                    );
                }
            }
            Ok(())
        })
    }
}
