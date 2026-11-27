use crate::*;

fn fixture() -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_slice(include_bytes!("../tests/forecast.json"))
}

#[test]
fn coordinates_reject_non_finite_values_and_keep_only_a_coarse_grid() -> Result<(), Error> {
    for (lat, lon) in [
        (f64::NAN, 0.0),
        (0.0, f64::INFINITY),
        (90.1, 0.0),
        (0.0, -180.1),
    ] {
        assert!(Coordinates::new(lat, lon).is_err());
    }
    let coordinates = Coordinates::new(38.72237, -9.13931)?;
    assert_eq!(coordinates.latitude(), 38.72);
    assert_eq!(coordinates.longitude(), -9.14);
    assert!(!format!("{coordinates:?}").contains("38.72"));
    Ok(())
}

#[test]
fn forecast_keeps_utc_epochs_and_zone_without_using_the_pcs_timezone() -> Result<(), Error> {
    let result = decode(include_bytes!("../tests/forecast.json"))?;
    assert_eq!(result.timezone, "Europe/Lisbon");
    assert_eq!(result.current.temperature_c, 17.0);
    assert_eq!(result.current.condition, Condition::PartlyCloudy);
    assert_eq!(result.current.is_day, Some(true));
    assert_eq!(
        result.hours.len(),
        2,
        "do not show an hour that already passed"
    );
    assert_eq!(result.hours[0].time, result.current.time);
    assert_eq!(result.days.len(), 2);
    Ok(())
}

#[test]
fn missing_values_never_become_zero_degrees_or_sunrise_at_midnight() -> Result<(), Error> {
    let mut raw = fixture()?;
    raw["hourly"]["temperature_2m"][1] = serde_json::Value::Null;
    raw["daily"]["temperature_2m_min"][1] = serde_json::Value::Null;
    raw["daily"]["sunrise"][0] = serde_json::Value::Null;
    raw["current"]["wind_speed_10m"] = serde_json::json!(-5);
    raw["current"]["weather_code"] = serde_json::json!(200);
    let result = decode(&serde_json::to_vec(&raw)?)?;
    assert_eq!(result.hours.len(), 1);
    assert_eq!(result.days.len(), 1);
    assert!(result.days[0].sunrise.is_none());
    assert!(result.current.wind_kmh.is_none());
    assert_eq!(result.current.condition, Condition::Unknown);
    raw["current"]["temperature_2m"] = serde_json::Value::Null;
    assert!(decode(&serde_json::to_vec(&raw)?).is_err());
    Ok(())
}

#[test]
fn mismatched_units_arrays_and_unordered_timelines_are_rejected() -> Result<(), Error> {
    let original = fixture()?;
    let mut units = original.clone();
    units["current_units"]["temperature_2m"] = serde_json::json!("°F");
    assert!(decode(&serde_json::to_vec(&units)?).is_err());
    let mut arrays = original.clone();
    arrays["hourly"]["weather_code"] = serde_json::json!([1]);
    assert!(decode(&serde_json::to_vec(&arrays)?).is_err());
    let mut timeline = original;
    timeline["hourly"]["time"][2] = timeline["hourly"]["time"][1].clone();
    assert!(decode(&serde_json::to_vec(&timeline)?).is_err());
    assert!(matches!(
        decode(&vec![b' '; 65 * 1024]),
        Err(Error::TooLarge)
    ));
    assert!(decode(b"not json").is_err());
    Ok(())
}

#[test]
fn unknown_codes_have_a_neutral_fallback_instead_of_inventing_sunshine() {
    for (code, expected) in [
        (0, Condition::Clear),
        (48, Condition::Fog),
        (57, Condition::FreezingRain),
        (63, Condition::Rain),
        (85, Condition::Snow),
        (81, Condition::Showers),
        (99, Condition::Thunderstorm),
        (200, Condition::Unknown),
    ] {
        assert_eq!(Condition::from_wmo(code), expected);
    }
}

#[test]
fn forecast_rejects_invalid_offset_and_out_of_range_calendar_timestamps()
-> Result<(), Box<dyn std::error::Error>> {
    let mut raw = fixture()?;
    raw["utc_offset_seconds"] = serde_json::json!(1000000);
    assert!(decode(&serde_json::to_vec(&raw)?).is_err());
    let mut raw = fixture()?;
    raw["daily"]["time"][1] = serde_json::json!(i64::MAX);
    assert!(decode(&serde_json::to_vec(&raw)?).is_err());
    Ok(())
}

#[test]
fn diagnostics_never_include_payloads_locations_or_urls() {
    assert_eq!(
        Error::Payload("private payload with coordinates").diagnostic_code(),
        "payload"
    );
    assert_eq!(Error::Http(403).diagnostic_code(), "http_status");
    assert_eq!(
        Error::RateLimited {
            retry_after: std::time::Duration::from_secs(300)
        }
        .diagnostic_code(),
        "rate_limited"
    );
}
