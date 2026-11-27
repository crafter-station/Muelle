//! Weather foundation, not a background service. This crate starts no threads,
//! timers or requests by itself. Blocking HTTP belongs on an opted-in worker;
//! location permission belongs to the foreground Settings UI, not this client.

mod client;
mod model;
mod policy;

pub use client::{Client, Error};
pub use model::{Condition, Coordinates, Current, Day, Forecast, Hour, decode};
pub use policy::{Cached, Demand, Refresh, Ticket};

pub const ATTRIBUTION: &str = "Weather data by Open-Meteo (CC BY 4.0)";
pub const ATTRIBUTION_URL: &str = "https://open-meteo.com/";

#[cfg(test)]
mod tests;
