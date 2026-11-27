//! Explicit city searches only: one bounded request, no background discovery.
use muelle_config::WeatherCity;
use serde::Deserialize;
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
pub struct CitySearch(Arc<AtomicBool>);
struct Busy(Arc<AtomicBool>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    results: Vec<Place>,
}
#[derive(Deserialize)]
struct Place {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    admin1: String,
    #[serde(default)]
    country: String,
}

fn query_valid(query: &str) -> bool {
    (2..=100).contains(&query.chars().count()) && !query.chars().any(char::is_control)
}

fn parse(bytes: &[u8]) -> Result<Vec<WeatherCity>, String> {
    let response: Response = serde_json::from_slice(bytes).map_err(|_| "unavailable")?;
    let mut cities = Vec::with_capacity(6);
    for place in response.results.into_iter().take(6) {
        if place.name.trim().is_empty() {
            continue;
        }
        let mut parts = Vec::with_capacity(3);
        for part in [&place.name, &place.admin1, &place.country] {
            let part = part.trim();
            if !part.is_empty() && !parts.contains(&part) {
                parts.push(part);
            }
        }
        let Some(city) =
            WeatherCity::from_degrees(parts.join(", "), place.latitude, place.longitude)
        else {
            continue;
        };
        if !cities.contains(&city) {
            cities.push(city);
        }
    }
    Ok(cities)
}

fn search(query: &str, language: &str) -> Result<Vec<WeatherCity>, String> {
    let language = match language {
        "es" | "fr" | "pt" | "zh" | "hi" | "ar" | "bn" => language,
        _ => "en",
    };
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .retry(reqwest::retry::never())
        .user_agent("Muelle/city-search")
        .build()
        .map_err(|_| "unavailable")?;
    let mut url = reqwest::Url::parse("https://geocoding-api.open-meteo.com/v1/search")
        .map_err(|_| "unavailable")?;
    url.query_pairs_mut().extend_pairs([
        ("name", query),
        ("count", "6"),
        ("language", language),
        ("format", "json"),
    ]);
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| "unavailable")?;
    let mut body = Vec::new();
    response
        .take(65_537)
        .read_to_end(&mut body)
        .map_err(|_| "unavailable")?;
    if body.len() > 65_536 {
        return Err("unavailable".into());
    }
    parse(&body)
}

#[tauri::command]
pub async fn search_weather_cities(
    query: String,
    language: String,
    state: tauri::State<'_, CitySearch>,
) -> Result<Vec<WeatherCity>, String> {
    let query = query.trim().to_owned();
    if !query_valid(&query) {
        return Err("query".into());
    }
    state
        .0
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "busy")?;
    let busy = Busy(state.0.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        search(&query, &language)
    })
    .await
    .map_err(|_| "unavailable".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_cities_are_validated_without_network() -> Result<(), String> {
        assert!(!query_valid("x"));
        assert!(!query_valid("ab\n"));
        assert!(!query_valid(&"a".repeat(101)));
        assert!(query_valid("Lima, PE"));
        let cities = parse(br#"{"results":[{"name":"Lima","admin1":"Lima","country":"Peru","latitude":-12.046,"longitude":-77.043},{"name":"Bad","latitude":91,"longitude":0},{"name":"","latitude":1,"longitude":1},{"name":"Lima","admin1":"Lima","country":"Peru","latitude":-12.046,"longitude":-77.043}]}"#)?;
        assert_eq!(cities.len(), 1);
        assert_eq!(cities[0].label, "Lima, Peru");
        assert_eq!(cities[0].latitude_e2, -1205);
        assert!(parse(br#"{}"#)?.is_empty());
        assert!(parse(b"not json").is_err());
        let flag = Arc::new(AtomicBool::new(true));
        drop(Busy(flag.clone()));
        assert!(!flag.load(Ordering::Acquire));
        Ok(())
    }
}
