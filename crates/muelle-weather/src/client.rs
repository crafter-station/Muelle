use crate::{Coordinates, Forecast, decode};
use std::{io::Read, time::Duration};

pub(crate) const MAX_BODY: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid weather coordinates")]
    Coordinates,
    #[error("invalid weather response: {0}")]
    Payload(&'static str),
    #[error("weather response exceeds 64 KiB")]
    TooLarge,
    #[error("weather provider rate limited the request")]
    RateLimited { retry_after: Duration },
    #[error("weather provider returned HTTP {0}")]
    Http(u16),
    #[error("weather network request failed: {0}")]
    Network(#[source] reqwest::Error),
    #[error("weather response could not be read: {0}")]
    Read(#[from] std::io::Error),
    #[error("weather JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Stable diagnostic category; never includes a URL, coordinate or body.
    pub fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Coordinates => "coordinates",
            Self::Payload(_) => "payload",
            Self::TooLarge => "body_limit",
            Self::RateLimited { .. } => "rate_limited",
            Self::Http(_) => "http_status",
            Self::Network(e) if e.is_timeout() => "network_timeout",
            Self::Network(_) => "network",
            Self::Read(_) => "body_read",
            Self::Json(_) => "json",
        }
    }
}

/// Lazily construct only after opt-in. Never invoke fetch on a UI thread.
/// A fixed HTTPS origin, no redirects, cookies, credentials or automatic retries.
pub struct Client(reqwest::blocking::Client);

impl Client {
    pub fn new() -> Result<Self, Error> {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(concat!(
                "Muelle/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/crafter-station/Muelle)"
            ))
            .build()
            .map_err(|error| Error::Network(error.without_url()))?;
        Ok(Self(client))
    }

    pub fn fetch(&self, coordinates: Coordinates) -> Result<Forecast, Error> {
        let mut url = reqwest::Url::parse("https://api.open-meteo.com/v1/forecast")
            .map_err(|_| Error::Payload("invalid fixed provider URL"))?;
        url.query_pairs_mut().extend_pairs([
            ("latitude", coordinates.latitude().to_string()),
            ("longitude", coordinates.longitude().to_string()),
            ("timezone", "auto".into()),
            ("timeformat", "unixtime".into()),
            ("temperature_unit", "celsius".into()),
            ("wind_speed_unit", "kmh".into()),
            ("forecast_days", "5".into()),
            ("forecast_hours", "24".into()),
            (
                "current",
                "temperature_2m,apparent_temperature,weather_code,is_day,wind_speed_10m".into(),
            ),
            (
                "hourly",
                "temperature_2m,precipitation_probability,weather_code,is_day".into(),
            ),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset".into(),
            ),
        ]);
        let response = self
            .0
            .get(url)
            .send()
            .map_err(|error| Error::Network(error.without_url()))?;
        if response.status().as_u16() == 429 {
            let header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |age| age.as_secs() as i64);
            return Err(Error::RateLimited {
                retry_after: retry_delay(header, now),
            });
        }
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BODY as u64)
        {
            return Err(Error::TooLarge);
        }
        let mut bytes = Vec::with_capacity(8 * 1024);
        response.take(MAX_BODY as u64 + 1).read_to_end(&mut bytes)?;
        let forecast = decode(&bytes)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Payload("system clock predates Unix epoch"))?
            .as_secs() as i64;
        if forecast.current.time < now - 3 * 3600 || forecast.current.time > now + 15 * 60 {
            return Err(Error::Payload(
                "current observation is outdated or in the future",
            ));
        }
        Ok(forecast)
    }
}

fn retry_delay(header: Option<&str>, now: i64) -> Duration {
    let seconds = header
        .and_then(|value| {
            value.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .map(|time| time.timestamp().saturating_sub(now).max(0) as u64)
            })
        })
        .unwrap_or(3600);
    Duration::from_secs(seconds.clamp(300, 24 * 3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_seconds_and_http_dates_and_bounds_bad_values() {
        let now = 1_445_412_480; // Wed, 21 Oct 2015 07:28:00 GMT
        for (header, seconds) in [
            (None, 3600),
            (Some("invalid"), 3600),
            (Some("0"), 300),
            (Some("7200"), 7200),
            (Some("999999999"), 86400),
            (Some("Wed, 21 Oct 2015 08:28:00 GMT"), 3600),
            (Some("Wed, 21 Oct 2015 06:28:00 GMT"), 300),
        ] {
            assert_eq!(
                retry_delay(header, now),
                Duration::from_secs(seconds),
                "{header:?}"
            );
        }
    }
}
