use crate::Error;
use serde::Deserialize;

/// Approximate coordinates only (0.01° grid). Never keep or send the precise
/// Windows fix for a city-scale weather forecast. Debug also hides the location.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Coordinates {
    latitude: i32,
    longitude: i32,
}

impl std::fmt::Debug for Coordinates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Coordinates(approximate, redacted)")
    }
}

impl Coordinates {
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, Error> {
        if !latitude.is_finite()
            || !longitude.is_finite()
            || !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
        {
            return Err(Error::Coordinates);
        }
        Ok(Self {
            latitude: (latitude * 100.0).round() as i32,
            longitude: (longitude * 100.0).round() as i32,
        })
    }
    pub fn latitude(self) -> f64 {
        f64::from(self.latitude) / 100.0
    }
    pub fn longitude(self) -> f64 {
        f64::from(self.longitude) / 100.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Clear,
    PartlyCloudy,
    Overcast,
    Fog,
    Drizzle,
    Rain,
    FreezingRain,
    Snow,
    Showers,
    Thunderstorm,
    Unknown,
}

impl Condition {
    pub fn from_wmo(code: u16) -> Self {
        match code {
            0 => Self::Clear,
            1 | 2 => Self::PartlyCloudy,
            3 => Self::Overcast,
            45 | 48 => Self::Fog,
            51 | 53 | 55 => Self::Drizzle,
            56 | 57 | 66 | 67 => Self::FreezingRain,
            61 | 63 | 65 => Self::Rain,
            71 | 73 | 75 | 77 | 85 | 86 => Self::Snow,
            80..=82 => Self::Showers,
            95 | 96 | 99 => Self::Thunderstorm,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug)]
pub struct Current {
    pub time: i64,
    pub temperature_c: f32,
    pub feels_like_c: Option<f32>,
    pub wind_kmh: Option<f32>,
    pub condition: Condition,
    pub is_day: Option<bool>,
}

#[derive(Debug)]
pub struct Hour {
    pub time: i64,
    pub temperature_c: f32,
    pub condition: Condition,
    pub precipitation_percent: Option<u8>,
}

#[derive(Debug)]
pub struct Day {
    pub time: i64,
    pub low_c: f32,
    pub high_c: f32,
    pub condition: Condition,
    /// Polar day/night or missing data have no invented sunrise/sunset.
    pub sunrise: Option<i64>,
    pub sunset: Option<i64>,
}

#[derive(Debug)]
pub struct Forecast {
    /// IANA zone, retained for DST-aware formatting in the future UI.
    pub timezone: String,
    pub current: Current,
    pub hours: Vec<Hour>,
    pub days: Vec<Day>,
}

#[derive(Deserialize)]
struct Units {
    temperature_2m: String,
    wind_speed_10m: String,
    time: String,
}
#[derive(Deserialize)]
struct RawCurrent {
    time: i64,
    temperature_2m: Option<f32>,
    apparent_temperature: Option<f32>,
    wind_speed_10m: Option<f32>,
    weather_code: Option<u16>,
    is_day: Option<u8>,
}
#[derive(Deserialize)]
struct RawHours {
    time: Vec<i64>,
    temperature_2m: Vec<Option<f32>>,
    weather_code: Vec<Option<u16>>,
    precipitation_probability: Vec<Option<u8>>,
}
#[derive(Deserialize)]
struct RawDays {
    time: Vec<i64>,
    temperature_2m_min: Vec<Option<f32>>,
    temperature_2m_max: Vec<Option<f32>>,
    weather_code: Vec<Option<u16>>,
    sunrise: Vec<Option<i64>>,
    sunset: Vec<Option<i64>>,
}
#[derive(Deserialize)]
struct Raw {
    timezone: String,
    current_units: Units,
    current: RawCurrent,
    hourly: RawHours,
    daily: RawDays,
}

fn temperature(value: Option<f32>) -> Option<f32> {
    value.filter(|value| value.is_finite() && (-100.0..=70.0).contains(value))
}

fn ascending(times: &[i64]) -> bool {
    times.iter().all(|&time| time > 0) && times.windows(2).all(|pair| pair[0] < pair[1])
}

/// Decode a single bounded response. Parallel arrays must match before indexing;
/// incomplete individual forecast rows are omitted, never represented as 0°.
pub fn decode(bytes: &[u8]) -> Result<Forecast, Error> {
    if bytes.len() > super::client::MAX_BODY {
        return Err(Error::TooLarge);
    }
    let raw: Raw = serde_json::from_slice(bytes)?;
    if raw.current_units.temperature_2m != "°C"
        || raw.current_units.wind_speed_10m != "km/h"
        || raw.current_units.time != "unixtime"
    {
        return Err(Error::Payload("unexpected weather units"));
    }
    if raw.timezone.is_empty()
        || raw.timezone.len() > 100
        || !raw
            .timezone
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_+-".contains(&c))
    {
        return Err(Error::Payload("invalid timezone"));
    }
    let h = raw.hourly;
    let d = raw.daily;
    let hn = h.time.len();
    let dn = d.time.len();
    if hn > 24
        || dn > 5
        || hn != h.temperature_2m.len()
        || hn != h.weather_code.len()
        || hn != h.precipitation_probability.len()
        || dn != d.temperature_2m_min.len()
        || dn != d.temperature_2m_max.len()
        || dn != d.weather_code.len()
        || dn != d.sunrise.len()
        || dn != d.sunset.len()
        || !ascending(&h.time)
        || !ascending(&d.time)
        || raw.current.time <= 0
    {
        return Err(Error::Payload("invalid forecast timelines"));
    }
    let current = Current {
        time: raw.current.time,
        temperature_c: temperature(raw.current.temperature_2m)
            .ok_or(Error::Payload("current temperature unavailable"))?,
        feels_like_c: temperature(raw.current.apparent_temperature),
        wind_kmh: raw
            .current
            .wind_speed_10m
            .filter(|v| v.is_finite() && (0.0..=500.0).contains(v)),
        condition: raw
            .current
            .weather_code
            .map(Condition::from_wmo)
            .unwrap_or(Condition::Unknown),
        is_day: raw.current.is_day.and_then(|day| match day {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }),
    };
    let hours = (0..hn)
        .filter_map(|i| {
            if h.time[i] < current.time {
                return None;
            }
            Some(Hour {
                time: h.time[i],
                temperature_c: temperature(h.temperature_2m[i])?,
                condition: h.weather_code[i]
                    .map(Condition::from_wmo)
                    .unwrap_or(Condition::Unknown),
                precipitation_percent: h.precipitation_probability[i].filter(|&v| v <= 100),
            })
        })
        .collect();
    let days = (0..dn)
        .filter_map(|i| {
            let low = temperature(d.temperature_2m_min[i])?;
            let high = temperature(d.temperature_2m_max[i])?;
            if low > high {
                return None;
            }
            Some(Day {
                time: d.time[i],
                low_c: low,
                high_c: high,
                condition: d.weather_code[i]
                    .map(Condition::from_wmo)
                    .unwrap_or(Condition::Unknown),
                sunrise: d.sunrise[i].filter(|&v| v > 0),
                sunset: d.sunset[i].filter(|&v| v > 0),
            })
        })
        .collect();
    Ok(Forecast {
        timezone: raw.timezone,
        current,
        hours,
        days,
    })
}
