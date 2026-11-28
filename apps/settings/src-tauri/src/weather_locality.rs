//! Resolve only a freshly consented device location. No IP fallback or polling.
use muelle_config::{WeatherCity, WeatherLocation};
use serde::Deserialize;
use std::{io::Read, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Locality {
    latitude: f64,
    longitude: f64,
    lookup_source: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    locality: String,
}

fn parse(bytes: &[u8], fix: WeatherLocation) -> Option<WeatherCity> {
    let result: Locality = serde_json::from_slice(bytes).ok()?;
    if !matches!(
        result.lookup_source.as_str(),
        "coordinates" | "reverseGeocoding"
    ) {
        return None;
    }
    let name = if result.city.trim().is_empty() {
        result.locality.trim()
    } else {
        result.city.trim()
    };
    let city = WeatherCity::from_degrees(name.into(), result.latitude, result.longitude)?;
    (city.latitude_e2 == fix.latitude_e2 && city.longitude_e2 == fix.longitude_e2).then_some(city)
}

/// A failed name lookup must never discard a valid Windows position. This is
/// called on the location worker, only after consent and cancellation checks.
pub fn resolve(fix: WeatherLocation, language: &str) -> Option<WeatherCity> {
    if let Some(city) = muelle_config::load(&muelle_config::default_path())
        .ok()
        .flatten()
        .and_then(|config| config.weather_auto_city)
        .filter(|city| city.latitude_e2 == fix.latitude_e2 && city.longitude_e2 == fix.longitude_e2)
    {
        return Some(city);
    }
    let language = match language {
        "es" | "en" | "fr" | "pt" | "zh" | "hi" | "ar" | "bn" => language,
        _ => "en",
    };
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(6))
        .user_agent(concat!("Muelle/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let mut url =
        reqwest::Url::parse("https://api.bigdatacloud.net/data/reverse-geocode-client").ok()?;
    url.query_pairs_mut().extend_pairs([
        (
            "latitude",
            format!("{:.2}", f64::from(fix.latitude_e2) / 100.0),
        ),
        (
            "longitude",
            format!("{:.2}", f64::from(fix.longitude_e2) / 100.0),
        ),
        ("localityLanguage", language.to_owned()),
    ]);
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .ok()?;
    let mut body = Vec::new();
    response.take(32_769).read_to_end(&mut body).ok()?;
    if body.len() > 32_768 {
        return None;
    }
    parse(&body, fix)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_bounded_matched_to_coordinates_and_never_inferred_from_ip()
    -> Result<(), &'static str> {
        let fix = WeatherLocation::from_degrees(-12.05, -77.04, 1).ok_or("fix")?;
        let valid = br#"{"latitude":-12.05,"longitude":-77.04,"lookupSource":"coordinates","city":"Lima","locality":"District"}"#;
        assert_eq!(parse(valid, fix).ok_or("city")?.label, "Lima");
        assert!(parse(br#"{"latitude":-12.05,"longitude":-77.04,"lookupSource":"reverseGeocoding","city":"Lima"}"#, fix).is_some());
        assert_eq!(parse(br#"{"latitude":-12.05,"longitude":-77.04,"lookupSource":"coordinates","city":"","locality":"Village"}"#, fix).ok_or("locality")?.label, "Village");
        assert!(parse(br#"{"latitude":-12.05,"longitude":-77.04,"lookupSource":"ip geolocation","city":"Lima"}"#, fix).is_none());
        assert!(
            parse(
                br#"{"latitude":0,"longitude":0,"lookupSource":"coordinates","city":"Lima"}"#,
                fix
            )
            .is_none()
        );
        assert!(parse(br#"{"latitude":-12.05,"longitude":-77.04,"lookupSource":"coordinates","city":"bad\nname"}"#, fix).is_none());
        assert!(parse(b"{}", fix).is_none());
        Ok(())
    }
}
