//! A bounded presentation built only on opening or a forecast/context change.
use super::text;
use crate::gfx::WeatherGlyph;
use anyhow::Result;
use muelle_config::Language;
use muelle_weather::Forecast;
use windows::{
    Foundation::DateTime, Globalization::DateTimeFormatting::DateTimeFormatter, core::HSTRING,
};
use windows_collections::IIterable;

#[derive(Default)]
pub struct Status {
    pub stale: bool,
    pub has_location: bool,
    pub failed: bool,
    pub offline: bool,
    pub saved: bool,
}

pub struct Hour {
    pub condition: &'static str,
    pub label: String,
    pub degrees: String,
    pub rain: String,
    pub glyph: WeatherGlyph,
}
pub struct Day {
    pub condition: &'static str,
    pub label: String,
    pub low: String,
    pub high: String,
    pub range: (f32, f32),
    pub glyph: WeatherGlyph,
}
pub struct Data {
    pub degrees: String,
    pub condition: &'static str,
    pub location: &'static str,
    pub status: &'static str,
    pub glyph: WeatherGlyph,
    pub feels: String,
    pub wind: String,
    pub hours: Vec<Hour>,
    pub days: Vec<Day>,
    pub sunrise: String,
    pub sunset: String,
    pub range: Option<(f32, f32)>,
}

struct Clock {
    hour: DateTimeFormatter,
    time: DateTimeFormatter,
    day: DateTimeFormatter,
    zone: HSTRING,
}
fn datetime(unix: i64) -> Option<DateTime> {
    Some(DateTime {
        UniversalTime: unix.checked_add(11_644_473_600)?.checked_mul(10_000_000)?,
    })
}
impl Clock {
    fn new(language: Language, zone: &str) -> Result<Self> {
        let locale = match language {
            Language::Es => "es",
            Language::Fr => "fr",
            Language::Pt => "pt",
            Language::Zh => "zh-Hans",
            Language::Hi => "hi",
            Language::Ar => "ar",
            Language::Bn => "bn",
            _ => "en",
        };
        let languages: IIterable<HSTRING> = vec![HSTRING::from(locale)].into();
        Ok(Self {
            hour: DateTimeFormatter::CreateDateTimeFormatterLanguages(
                &HSTRING::from("hour"),
                &languages,
            )?,
            time: DateTimeFormatter::CreateDateTimeFormatterLanguages(
                &HSTRING::from("hour minute"),
                &languages,
            )?,
            day: DateTimeFormatter::CreateDateTimeFormatterLanguages(
                &HSTRING::from("dayofweek.abbreviated"),
                &languages,
            )?,
            zone: HSTRING::from(zone),
        })
    }
    fn hour(&self, unix: i64) -> String {
        datetime(unix)
            .and_then(|stamp| self.hour.FormatUsingTimeZone(stamp, &self.zone).ok())
            .map_or_else(|| "—".into(), |s| s.to_string())
    }
    fn format(&self, unix: i64, day: bool, offset: i32) -> String {
        let stamp = if day {
            unix.checked_add(i64::from(offset))
        } else {
            Some(unix)
        };
        stamp
            .and_then(datetime)
            .and_then(|stamp| {
                if day {
                    self.day
                        .FormatUsingTimeZone(stamp, &HSTRING::from("Etc/UTC"))
                        .ok()
                } else {
                    self.time.FormatUsingTimeZone(stamp, &self.zone).ok()
                }
            })
            .map_or_else(|| "—".into(), |s| s.to_string())
    }
}
fn degrees(value: f32) -> String {
    format!("{:.0}°", value.round())
}
impl Data {
    pub fn new(forecast: Option<&Forecast>, language: Language, now: i64, state: Status) -> Self {
        let Status {
            stale,
            has_location,
            failed,
            offline,
            saved,
        } = state;
        let stale = stale || forecast.is_some_and(|f| now.saturating_sub(f.current.time) > 30 * 60);
        let t = crate::weather::text::text(language);
        let p = text::text(language);
        let location = if saved {
            t.saved_location
        } else {
            t.current_location
        };
        let status = if !has_location {
            t.location_needed
        } else if offline {
            p.offline
        } else if stale {
            t.stale
        } else if forecast.is_none() && failed {
            t.unavailable
        } else if forecast.is_none() {
            t.loading
        } else {
            ""
        };
        let mut view = Self {
            degrees: "—".into(),
            condition: if forecast.is_none() {
                status
            } else {
                t.unavailable
            },
            location,
            status,
            glyph: WeatherGlyph::Unknown,
            feels: "—".into(),
            wind: "—".into(),
            hours: Vec::new(),
            days: Vec::new(),
            sunrise: "—".into(),
            sunset: "—".into(),
            range: None,
        };
        let Some(forecast) = forecast else {
            return view;
        };
        let clock = Clock::new(language, &forecast.timezone).ok();
        let time = |unix, day| {
            clock.as_ref().map_or_else(
                || "—".into(),
                |c| c.format(unix, day, forecast.utc_offset_seconds),
            )
        };
        view.degrees = degrees(forecast.current.temperature_c);
        view.condition = t.condition(forecast.current.condition);
        view.glyph = crate::weather::glyph(forecast.current.condition, forecast.current.is_day);
        view.feels = forecast
            .current
            .feels_like_c
            .map_or_else(|| "—".into(), degrees);
        view.wind = forecast
            .current
            .wind_kmh
            .map_or_else(|| "—".into(), |v| format!("{v:.0} km/h"));
        // Only a fresh observation is described as "Now"; old data is marked
        // explicitly and never masquerades as a new current measurement.
        if !stale && !offline {
            view.hours.push(Hour {
                condition: view.condition,
                label: p.now.into(),
                degrees: view.degrees.clone(),
                rain: "—".into(),
                glyph: view.glyph,
            });
        }
        for hour in forecast
            .hours
            .iter()
            .filter(|h| h.time > now)
            .take(6 - view.hours.len())
        {
            view.hours.push(Hour {
                condition: t.condition(hour.condition),
                label: clock
                    .as_ref()
                    .map_or_else(|| "—".into(), |c| c.hour(hour.time)),
                degrees: degrees(hour.temperature_c),
                rain: hour
                    .precipitation_percent
                    .map_or_else(|| "—".into(), |p| format!("{p}%")),
                glyph: crate::weather::glyph(hour.condition, hour.is_day),
            });
        }
        let calendar_day =
            |stamp: i64| (stamp + i64::from(forecast.utc_offset_seconds)).div_euclid(86400);
        for day in forecast
            .days
            .iter()
            .filter(|d| calendar_day(d.time) >= calendar_day(now))
            .take(5)
        {
            let today = calendar_day(day.time) == calendar_day(now);
            if today {
                view.sunrise = day.sunrise.map_or_else(|| "—".into(), |v| time(v, false));
                view.sunset = day.sunset.map_or_else(|| "—".into(), |v| time(v, false));
            }
            view.days.push(Day {
                condition: t.condition(day.condition),
                label: if today {
                    p.today.into()
                } else {
                    time(day.time, true)
                },
                low: degrees(day.low_c),
                high: degrees(day.high_c),
                range: (day.low_c, day.high_c),
                glyph: crate::weather::glyph(day.condition, Some(true)),
            });
        }
        view.range = view
            .days
            .iter()
            .map(|d| d.range)
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_is_bounded_omits_past_hours_and_marks_old_data() -> Result<()> {
        let forecast = muelle_weather::decode(include_bytes!(
            "../../../../../crates/muelle-weather/tests/forecast.json"
        ))?;
        let now = forecast.current.time;
        let data = Data::new(
            Some(&forecast),
            Language::Es,
            now,
            Status {
                has_location: true,
                ..Default::default()
            },
        );
        assert!(data.hours.len() <= 6 && data.days.len() <= 5);
        assert_eq!(data.hours[0].label, "Ahora");
        assert_eq!(data.days[0].label, "Hoy");
        let old = Data::new(
            Some(&forecast),
            Language::Es,
            now + 3600,
            Status {
                stale: true,
                has_location: true,
                ..Default::default()
            },
        );
        assert_eq!(old.status, "Datos anteriores");
        assert!(old.hours.iter().all(|h| h.label != "Ahora"));
        let missing = Data::new(None, Language::Es, now, Status::default());
        assert!(missing.hours.is_empty() && missing.days.is_empty());
        assert_eq!(missing.degrees, "—");
        let offline = Data::new(
            Some(&forecast),
            Language::Es,
            now,
            Status {
                has_location: true,
                offline: true,
                ..Default::default()
            },
        );
        assert_eq!(offline.status, "Sin conexión");
        assert!(offline.hours.iter().all(|h| h.label != "Ahora"));
        Ok(())
    }
    #[test]
    fn windows_formats_iana_hours_across_midnight_and_dst_without_a_pc_zone() -> Result<()> {
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    windows::Win32::System::WinRT::RoUninitialize();
                }
            }
        }
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )?;
        }
        let _com = Com;
        let clock = Clock::new(Language::Es, "America/New_York")?;
        // 2026-11-01 05:30 / 06:30 UTC: repeated 01:30 at the DST transition.
        let a = chrono::DateTime::parse_from_rfc3339("2026-11-01T05:30:00Z")?.timestamp();
        let b = a + 3600;
        assert_eq!(clock.format(a, false, 0), clock.format(b, false, 0));
        assert_ne!(clock.format(a, false, 0), "—");
        assert_ne!(clock.hour(a), "—");
        assert_ne!(clock.format(a, true, -4 * 3600), "—");
        assert!(datetime(i64::MAX).is_none());
        Ok(())
    }
}
