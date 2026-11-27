//! Native retained weather card; forecast workers/scheduling live in app/weather.
#[path = "weather/card.rs"]
mod card;
#[path = "weather/placement.rs"]
mod placement;
#[path = "weather/text.rs"]
pub mod text;
pub use card::{Card, Key, Model};
pub use placement::{Placement, place, reserve};

#[path = "weather/popup.rs"]
pub mod popup;

use crate::gfx::WeatherGlyph;
use muelle_weather::Condition;

pub fn glyph(condition: Condition, is_day: Option<bool>) -> WeatherGlyph {
    match condition {
        Condition::Clear if is_day == Some(false) => WeatherGlyph::Moon,
        Condition::Clear => WeatherGlyph::Sun,
        Condition::PartlyCloudy if is_day == Some(false) => WeatherGlyph::NightCloud,
        Condition::PartlyCloudy => WeatherGlyph::PartlyCloudy,
        Condition::Overcast => WeatherGlyph::Cloud,
        Condition::Fog => WeatherGlyph::Fog,
        Condition::Drizzle | Condition::Rain | Condition::FreezingRain | Condition::Showers => {
            WeatherGlyph::Rain
        }
        Condition::Snow => WeatherGlyph::Snow,
        Condition::Thunderstorm => WeatherGlyph::Storm,
        Condition::Unknown => WeatherGlyph::Unknown,
    }
}

/// Only for weather-owned trees: no brushes from the Dock/asset cache enter them.
/// Detach first, then explicitly release surfaces on the shared compositor.
fn close_tree(root: &windows::UI::Composition::ContainerVisual) {
    use windows::{
        UI::Composition::{
            CompositionDrawingSurface, CompositionSurfaceBrush, ContainerVisual, SpriteVisual,
            Visual,
        },
        core::Interface,
    };
    fn close(visual: &Visual) {
        if let Ok(container) = visual.cast::<ContainerVisual>()
            && let Ok(collection) = container.Children()
        {
            let children: Vec<_> = (&collection).into_iter().collect();
            let _ = collection.RemoveAll();
            for child in children {
                close(&child);
            }
        }
        if let Ok(sprite) = visual.cast::<SpriteVisual>()
            && let Ok(brush) = sprite.Brush()
        {
            let _ = sprite.SetBrush(None::<&windows::UI::Composition::CompositionBrush>);
            if let Ok(surface_brush) = brush.cast::<CompositionSurfaceBrush>()
                && let Ok(surface) = surface_brush.Surface()
                && let Ok(drawing) = surface.cast::<CompositionDrawingSurface>()
            {
                let _ = drawing.Close();
            }
            let _ = brush.Close();
        }
        let _ = visual.Close();
    }
    if let Ok(visual) = root.cast::<Visual>() {
        close(&visual);
    }
}
