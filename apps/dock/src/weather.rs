//! Native retained weather card; forecast workers/scheduling live in app/weather.
#[path = "weather/card.rs"]
mod card;
#[path = "weather/placement.rs"]
mod placement;
#[path = "weather/text.rs"]
pub mod text;
pub use card::{Card, Key, Model};
pub use placement::{Placement, place};
