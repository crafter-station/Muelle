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
    /// None identifies the current observation, not an hourly forecast row.
    pub time: Option<i64>,
    pub selected: bool,
    pub condition: &'static str,
    pub label: String,
    pub degrees: String,
    pub rain: String,
    pub glyph: WeatherGlyph,
}
pub struct Day {
    pub time: i64,
    pub selected: bool,
    pub condition: &'static str,
    pub label: String,
    pub low: String,
    pub high: String,
    pub range: (f32, f32),
    pub glyph: WeatherGlyph,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Selection {
    pub day: Option<i64>,
    pub page: usize,
    pub hour: Option<i64>,
}

pub struct Data {
    pub selection: Selection,
    pub title: String,
    pub hourly_title: String,
    pub future: bool,
    pub previous_hours: bool,
    pub next_hours: bool,
    pub degrees: String,
    pub condition: &'static str,
    pub location: String,
    pub status: &'static str,
    pub glyph: WeatherGlyph,
    pub feels: String,
    pub wind: String,
    pub hours: Vec<Hour>,
    pub days: Vec<Day>,
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
    #[cfg(test)]
    pub fn new(forecast: Option<&Forecast>, language: Language, now: i64, state: Status) -> Self {
        Self::for_selection(forecast, language, now, state, Selection::default())
    }
    pub fn for_selection(
        forecast: Option<&Forecast>,
        language: Language,
        now: i64,
        state: Status,
        selection: Selection,
    ) -> Self {
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
            selection: Selection::default(),
            title: t.title.into(),
            hourly_title: p.hourly.into(),
            future: false,
            previous_hours: false,
            next_hours: false,
            degrees: "—".into(),
            condition: if forecast.is_none() {
                status
            } else {
                t.unavailable
            },
            location: location.into(),
            status,
            glyph: WeatherGlyph::Unknown,
            feels: "—".into(),
            wind: "—".into(),
            hours: Vec::new(),
            days: Vec::new(),
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
        let calendar_day =
            |stamp: i64| (stamp + i64::from(forecast.utc_offset_seconds)).div_euclid(86400);
        let today = calendar_day(now);
        let days: Vec<_> = forecast
            .days
            .iter()
            .filter(|d| calendar_day(d.time) >= today)
            .take(5)
            .collect();
        let selected = days
            .iter()
            .copied()
            .find(|d| Some(d.time) == selection.day)
            .or_else(|| days.first().copied());
        let selected_time = selected.map(|d| d.time);
        let selected_calendar = selected.map_or(today, |d| calendar_day(d.time));
        view.future = selected_calendar > today;
        view.selection.day = selected_time;
        if let Some(day) = selected
            && view.future
        {
            let label = time(day.time, true);
            view.title = format!("{} · {}", t.title, label);
            view.hourly_title = format!("{} · {}", p.hours, label);
            // A future day is not a current observation. Its main icon and
            // details are filled from the selected hourly row below.
            view.degrees = "—".into();
            view.condition = t.unavailable;
            view.glyph = WeatherGlyph::Unknown;
            view.wind = "—".into();
            view.feels = "—".into();
        }
        // Paging reuses the same five-day snapshot, never the network. Bound
        // both iteration and output even if a caller supplies a bogus page.
        let include_now = !view.future && !stale && !offline;
        let eligible = || {
            forecast
                .hours
                .iter()
                .filter(|h| h.time > now && calendar_day(h.time) == selected_calendar)
        };
        let total = eligible().count() + usize::from(include_now);
        let requested_page = if selection.day == selected_time {
            selection.page
        } else {
            0
        };
        let page = requested_page.min(total.saturating_sub(1) / 6);
        view.selection.page = page;
        view.previous_hours = page > 0;
        view.next_hours = (page + 1) * 6 < total;
        if include_now && page == 0 {
            view.hours.push(Hour {
                time: None,
                selected: false,
                condition: view.condition,
                label: p.now.into(),
                degrees: view.degrees.clone(),
                rain: "—".into(),
                glyph: view.glyph,
            });
        }
        for hour in eligible()
            .skip((page * 6).saturating_sub(usize::from(include_now)))
            .take(6 - view.hours.len())
        {
            view.hours.push(Hour {
                time: Some(hour.time),
                selected: false,
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
        // Only select a row on this visible page. A changed page/day or an
        // expired timestamp cannot leave a hidden hour driving the header.
        let active = view
            .hours
            .iter()
            .position(|h| h.time == selection.hour)
            .unwrap_or(0);
        if let Some(hour) = view.hours.get_mut(active) {
            hour.selected = true;
            view.selection.hour = hour.time;
            if let Some(stamp) = hour.time {
                view.degrees.clone_from(&hour.degrees);
                view.condition = hour.condition;
                view.glyph = hour.glyph;
                view.title = format!(
                    "{} · {} · {}",
                    t.title,
                    selected.map_or_else(
                        || p.today.into(),
                        |d| if calendar_day(d.time) == today {
                            p.today.into()
                        } else {
                            time(d.time, true)
                        }
                    ),
                    hour.label
                );
                if let Some(source) = forecast.hours.iter().find(|h| h.time == stamp) {
                    view.wind = source
                        .wind_kmh
                        .map_or_else(|| "—".into(), |v| format!("{v:.0} km/h"));
                    view.feels = source.feels_like_c.map_or_else(|| "—".into(), degrees);
                }
            }
        }
        for day in days {
            view.days.push(Day {
                time: day.time,
                selected: Some(day.time) == selected_time,
                condition: t.condition(day.condition),
                label: if calendar_day(day.time) == today {
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
    fn current_and_selected_hours_never_take_the_daily_weather_code() -> Result<()> {
        use muelle_weather::Condition;
        let mut forecast = muelle_weather::decode(include_bytes!(
            "../../../../../crates/muelle-weather/tests/forecast.json"
        ))?;
        let now = forecast.current.time;
        forecast.current.condition = Condition::Clear;
        forecast.current.is_day = Some(true);
        forecast.days[0].condition = Condition::Thunderstorm;
        forecast.hours[1].condition = Condition::Snow;
        forecast.hours[1].temperature_c = -2.0;
        forecast.hours[1].wind_kmh = Some(8.0);
        forecast.hours[1].feels_like_c = Some(-4.0);
        let status = || Status {
            has_location: true,
            ..Default::default()
        };
        let current = Data::new(Some(&forecast), Language::Es, now, status());
        assert_eq!(current.glyph, WeatherGlyph::Sun);
        assert_eq!(current.days[0].glyph, WeatherGlyph::Storm);
        assert_eq!(current.hours[0].glyph, WeatherGlyph::Sun);
        let target = forecast.hours[1].time;
        let selected = Data::for_selection(
            Some(&forecast),
            Language::Es,
            now,
            status(),
            Selection {
                day: Some(forecast.days[0].time),
                page: 0,
                hour: Some(target),
            },
        );
        assert_eq!(selected.glyph, WeatherGlyph::Snow);
        assert_eq!(selected.degrees, "-2°");
        assert_eq!(selected.wind, "8 km/h");
        assert_eq!(selected.feels, "-4°");
        assert_eq!(selected.selection.hour, Some(target));
        assert_eq!(selected.hours.iter().filter(|h| h.selected).count(), 1);
        // Selecting Now restores the observation instead of the day's storm.
        let again = Data::for_selection(
            Some(&forecast),
            Language::Es,
            now,
            status(),
            Selection {
                hour: None,
                ..selected.selection
            },
        );
        assert_eq!(again.glyph, WeatherGlyph::Sun);
        assert!(again.hours[0].selected);
        forecast.hours[1].condition = Condition::Clear;
        forecast.hours[1].is_day = Some(false);
        let night = Data::for_selection(
            Some(&forecast),
            Language::Es,
            now,
            status(),
            selected.selection,
        );
        assert_eq!(
            night.glyph,
            WeatherGlyph::Moon,
            "hourly night must not inherit today's sun or storm"
        );
        Ok(())
    }
    #[test]
    fn day_selection_updates_details_pages_and_never_reuses_current_values() -> Result<()> {
        let mut forecast = muelle_weather::decode(include_bytes!(
            "../../../../../crates/muelle-weather/tests/forecast.json"
        ))?;
        let selected = forecast.days[1].time;
        forecast.days[1].wind_max_kmh = Some(25.0);
        forecast.days[1].precipitation_percent = Some(65);
        forecast.hours = (0..24)
            .map(|hour| muelle_weather::Hour {
                time: selected + hour * 3600,
                temperature_c: hour as f32,
                feels_like_c: Some(hour as f32 - 1.0),
                wind_kmh: Some(hour as f32 + 3.0),
                condition: muelle_weather::Condition::Rain,
                precipitation_percent: Some(hour as u8),
                is_day: Some((6..18).contains(&hour)),
            })
            .collect();
        let data = Data::for_selection(
            Some(&forecast),
            Language::Es,
            forecast.current.time,
            Status {
                has_location: true,
                ..Default::default()
            },
            Selection {
                day: Some(selected),
                page: 0,
                hour: None,
            },
        );
        assert!(data.future);
        assert_eq!(data.degrees, "0°");
        assert_eq!(data.wind, "3 km/h");
        assert_eq!(data.feels, "-1°");
        assert_eq!(data.glyph, WeatherGlyph::Rain);
        assert_ne!(data.glyph, data.days[1].glyph);
        assert!(data.hours[0].selected);
        assert_eq!(data.selection.hour, Some(selected));
        assert!(data.days[1].selected && !data.days[0].selected);
        assert_eq!(data.hours.len(), 6);
        assert!(data.next_hours && !data.previous_hours);
        assert!(data.hours.iter().all(|h| h.label != "Ahora"));
        let page = Data::for_selection(
            Some(&forecast),
            Language::Es,
            forecast.current.time,
            Status {
                has_location: true,
                ..Default::default()
            },
            Selection {
                day: Some(selected),
                page: 999,
                hour: None,
            },
        );
        assert_eq!(page.selection.page, 3);
        assert_eq!(page.hours[0].degrees, "18°");
        assert!(page.previous_hours && !page.next_hours);
        assert!(data.title.contains("Clima") && data.hourly_title.contains("Por horas"));
        let rolled = Data::for_selection(
            Some(&forecast),
            Language::Es,
            forecast.current.time,
            Status {
                has_location: true,
                ..Default::default()
            },
            Selection {
                day: Some(1),
                page: 999,
                hour: None,
            },
        );
        assert_eq!(rolled.selection.day, Some(forecast.days[0].time));
        assert!(!rolled.future);
        Ok(())
    }
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
