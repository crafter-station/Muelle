//! Native textual forecast for assistive technology/high contrast. Only on click.
use super::*;
impl App {
    pub(super) fn accessible_weather_menu(&mut self) -> Result<Option<Outside>> {
        let Some(context) = self.weather_popup_context() else {
            return Ok(None);
        };
        self.close_weather_popup();
        let data = self.weather_popup_data(context);
        let items = items(&data, context.language);
        let r = context.anchor;
        let (anchor, placement) = match context.edge {
            muelle_core::frame::DockEdge::Bottom => (
                POINT {
                    x: (r.left + r.right) / 2,
                    y: r.top,
                },
                menu::Placement::Above,
            ),
            muelle_core::frame::DockEdge::Left => (
                POINT {
                    x: r.right,
                    y: (r.top + r.bottom) / 2,
                },
                menu::Placement::Right,
            ),
            muelle_core::frame::DockEdge::Right => (
                POINT {
                    x: r.left,
                    y: (r.top + r.bottom) / 2,
                },
                menu::Placement::Left,
            ),
        };
        Ok(Some(Outside::Menu(MenuRequest {
            gfx: self.gfx.clone(),
            target: MenuTarget::Weather,
            items,
            anchor,
            placement,
            look: context.look,
        })))
    }
}

fn items(data: &weather::popup::data::Data, language: muelle_config::Language) -> Vec<MenuItem> {
    let t = weather::text::text(language);
    let words = weather::popup::text::text(language);
    let mut lines = vec![format!(
        "{} · {} · {}",
        data.location, data.condition, data.degrees
    )];
    if !data.status.is_empty() {
        lines.push(data.status.into());
    }
    lines.push(format!(
        "{}: {} · {}: {}",
        words.feels, data.feels, words.wind, data.wind
    ));
    for hour in &data.hours {
        lines.push(format!(
            "{} · {} · {} · {} {}",
            hour.label, hour.condition, hour.degrees, words.rain, hour.rain
        ));
    }
    for day in &data.days {
        lines.push(format!(
            "{} · {} · {} / {}",
            day.label, day.condition, day.low, day.high
        ));
    }
    let mut items = vec![MenuItem::Header(t.title.into())];
    // Enabled native rows can be reached/read by keyboard and Narrator.
    // Selecting a forecast line simply dismisses it; no external operation.
    items.extend(
        lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| MenuItem::action(100 + i as u32, line.replace('&', "&&"))),
    );
    items.extend([
        MenuItem::Separator,
        MenuItem::action(SETTINGS, t.settings),
        MenuItem::action(SOURCE, muelle_weather::ATTRIBUTION),
    ]);
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use weather::popup::data::{Data, Status};
    #[test]
    fn native_forecast_rows_are_keyboard_reachable_and_have_no_side_effect_commands() -> Result<()>
    {
        let forecast = muelle_weather::decode(include_bytes!(
            "../../../../../crates/muelle-weather/tests/forecast.json"
        ))?;
        let data = Data::new(
            Some(&forecast),
            muelle_config::Language::Es,
            forecast.current.time,
            Status {
                has_location: true,
                ..Default::default()
            },
        );
        let rows = items(&data, muelle_config::Language::Es);
        let mut info = 0;
        for row in rows {
            if let MenuItem::Action {
                command,
                enabled,
                label,
                ..
            } = row
            {
                assert!(
                    enabled,
                    "forecast rows must be reachable to Narrator via native keyboard navigation"
                );
                assert!(!label.is_empty());
                assert!(command == SETTINGS || command == SOURCE || command >= 100);
                if command >= 100 {
                    info += 1;
                }
            }
        }
        assert_eq!(info, 2 + data.hours.len() + data.days.len());
        Ok(())
    }
}
