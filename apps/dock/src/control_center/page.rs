//! The page a held tile opens into: a title, perhaps a switch, a slider or
//! the player, and rows to choose from. What it holds and where each part goes
//! is decided without drawing, so it can be tested.

use super::{
    GRID, HEIGHT, Launch, Look, Painter, Slider, Target, Tile, around, contains, glyph, pixels,
};
use crate::{media, strings::Strings};
use anyhow::Result;
use muelle_shell::control_lists::{WifiList, WifiNetwork};

const PAD: f32 = 16.0;
const HEADER: f32 = 48.0;
const SLIDER: f32 = 34.0;
const PLAYER: f32 = 164.0;
const ROW: f32 = 36.0;
const ROW_WITH_NOTE: f32 = 44.0;
const DIVIDER: f32 = 9.0;
const BOTTOM: f32 = 8.0;
/// Saved networks listed while the rest are hidden.
const SAVED_SHOWN: usize = 6;
const ART: f32 = 72.0;
/// The player's buttons, from the page's centre.
const TRANSPORT: [(Target, f32); 3] = [
    (Target::Previous, -56.0),
    (Target::PlayPause, 0.0),
    (Target::Next, 56.0),
];
const TRANSPORT_Y: f32 = HEADER + 136.0;
const PROGRESS_Y: f32 = HEADER + 90.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Network,
    Media,
    Bluetooth,
    Sharing,
    Focus,
    Desktops,
    Screens,
    Display,
    Sound,
    Appearance,
    Screenshot,
}

/// The page a tile opens into, if it has one.
pub fn page_of(tile: Tile) -> Option<Page> {
    Some(match tile {
        Tile::Network => Page::Network,
        Tile::Media => Page::Media,
        Tile::Bluetooth => Page::Bluetooth,
        Tile::Share => Page::Sharing,
        Tile::Focus => Page::Focus,
        Tile::TaskView => Page::Desktops,
        Tile::Cast => Page::Screens,
        Tile::Display => Page::Display,
        Tile::Sound => Page::Sound,
        Tile::DarkMode => Page::Appearance,
        Tile::Screenshot => Page::Screenshot,
        // Only one thing to do: a click does it.
        Tile::Calculator | Tile::Timer | Tile::More => return None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Launch(Launch),
    /// Join the Wi-Fi network with this saved profile.
    JoinWifi(String),
    /// Play sound through the output with this id.
    SelectOutput(String),
    Dark(bool),
    /// Turn Do Not Disturb over.
    Focus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub symbol: Option<char>,
    pub label: String,
    pub note: Option<String>,
    /// Marked as the one in use.
    pub checked: bool,
    pub action: Option<Action>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    Wifi,
    Bluetooth,
    /// Do Not Disturb.
    Focus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub title: &'static str,
    pub switch: Option<(Switch, bool)>,
    /// `Target::BrightnessSlider` or `Target::VolumeSlider`, and its value.
    pub slider: Option<(Target, Option<u8>)>,
    pub player: bool,
    pub rows: Vec<Row>,
    /// Under a divider: where Windows has the rest.
    pub footer: Option<Row>,
}

fn choice(symbol: char, label: &str, action: Action) -> Row {
    Row {
        symbol: Some(symbol),
        label: label.to_owned(),
        note: None,
        checked: false,
        action: Some(action),
    }
}

fn launch(symbol: char, label: &str, target: Launch) -> Row {
    choice(symbol, label, Action::Launch(target))
}

/// A line of explanation; pressing it does nothing.
fn message(label: &str) -> Row {
    Row {
        symbol: None,
        label: label.to_owned(),
        note: None,
        checked: false,
        action: None,
    }
}

fn link(label: &str, target: Launch) -> Row {
    Row {
        symbol: None,
        ..launch(glyph::NEXT, label, target)
    }
}

pub fn content(page: Page, look: &Look, text: &'static Strings) -> Content {
    let mut content = Content {
        title: "",
        switch: None,
        slider: None,
        player: false,
        rows: Vec::new(),
        footer: None,
    };
    match page {
        Page::Network => {
            content.title = "Wi-Fi";
            content.switch = look.readings.wifi.map(|on| (Switch::Wifi, on));
            content.rows = network_rows(look, text);
            content.footer = Some(link(text.control_wifi_settings, Launch::WifiSettings));
        }
        Page::Bluetooth => {
            content.title = "Bluetooth";
            content.switch = look.readings.bluetooth.map(|on| (Switch::Bluetooth, on));
            content.rows = match (look.readings.bluetooth, &look.lists.bluetooth) {
                (None, Some(_)) => vec![message(text.control_no_bluetooth)],
                (_, None) => vec![message(text.control_searching)],
                (_, Some(devices)) if devices.is_empty() => {
                    vec![message(text.control_no_devices)]
                }
                (on, Some(devices)) => devices
                    .iter()
                    .map(|device| {
                        let connected = device.connected && on == Some(true);
                        Row {
                            note: connected.then(|| text.control_connected.to_owned()),
                            checked: connected,
                            // Windows connects devices from its own page.
                            ..launch(glyph::BLUETOOTH, &device.name, Launch::BluetoothSettings)
                        }
                    })
                    .collect(),
            };
            content.footer = Some(link(
                text.control_bluetooth_settings,
                Launch::BluetoothSettings,
            ));
        }
        Page::Sharing => {
            content.title = text.control_nearby;
            content.rows = vec![launch(
                glyph::SHARE,
                text.control_sharing_settings,
                Launch::NearbySharing,
            )];
        }
        Page::Focus => {
            content.title = text.control_focus;
            content.rows = vec![
                Row {
                    checked: look.readings.focus == Some(true),
                    // Without the switch the row only names it.
                    action: look.readings.focus.map(|_| Action::Focus),
                    ..choice(glyph::FOCUS, text.control_dnd, Action::Focus)
                },
                launch(glyph::TIMER, text.control_focus_session, Launch::Timer),
                launch(
                    glyph::NOTIFICATIONS,
                    text.control_notification_settings,
                    Launch::Notifications,
                ),
            ];
            content.footer = Some(link(text.control_focus_settings, Launch::Focus));
        }
        Page::Desktops => {
            content.title = text.control_desktops;
            content.rows = vec![
                launch(glyph::TASK_VIEW, text.control_task_view, Launch::TaskView),
                launch(glyph::ADD, text.control_new_desktop, Launch::NewDesktop),
                launch(
                    glyph::BACK,
                    text.control_previous_desktop,
                    Launch::PreviousDesktop,
                ),
                launch(
                    glyph::FORWARD,
                    text.control_next_desktop,
                    Launch::NextDesktop,
                ),
                launch(
                    glyph::DESKTOP,
                    text.control_show_desktop,
                    Launch::ShowDesktop,
                ),
            ];
        }
        Page::Screens => {
            content.title = text.control_screen;
            content.rows = vec![
                launch(glyph::CAST, text.control_cast_device, Launch::Cast),
                launch(
                    glyph::DESKTOP,
                    text.control_this_screen,
                    Launch::Project(Projection::ThisScreen),
                ),
                launch(
                    glyph::TASK_VIEW,
                    text.control_duplicate,
                    Launch::Project(Projection::Duplicate),
                ),
                launch(
                    glyph::EXTEND,
                    text.control_extend,
                    Launch::Project(Projection::Extend),
                ),
                launch(
                    glyph::CAST,
                    text.control_second_screen,
                    Launch::Project(Projection::SecondScreen),
                ),
            ];
            content.footer = Some(link(text.control_display_settings, Launch::DisplaySettings));
        }
        Page::Display => {
            content.title = text.control_display;
            content.slider = Some((Target::BrightnessSlider, look.readings.brightness));
            let dark = look.readings.dark;
            content.rows = vec![
                Row {
                    checked: dark,
                    ..choice(
                        glyph::DARK_MODE,
                        text.control_dark_mode,
                        Action::Dark(!dark),
                    )
                },
                launch(glyph::NIGHT, text.control_night_light, Launch::NightLight),
            ];
            content.footer = Some(link(text.control_display_settings, Launch::DisplaySettings));
        }
        Page::Appearance => {
            content.title = text.control_appearance;
            content.rows = [
                (false, glyph::BRIGHTNESS, text.control_light),
                (true, glyph::NIGHT, text.control_dark),
            ]
            .into_iter()
            .map(|(dark, symbol, label)| Row {
                checked: look.readings.dark == dark,
                ..choice(symbol, label, Action::Dark(dark))
            })
            .collect();
            content.footer = Some(link(text.control_personalize, Launch::Personalize));
        }
        Page::Sound => {
            content.title = text.control_sound;
            content.slider = Some((
                Target::VolumeSlider,
                look.volume.map(|volume| volume.percent),
            ));
            content.rows = match &look.lists.outputs {
                None => vec![message(text.control_searching)],
                Some(outputs) if outputs.is_empty() => vec![message(text.media_no_output)],
                Some(outputs) => outputs
                    .iter()
                    .map(|output| Row {
                        checked: output.selected,
                        ..choice(
                            glyph::OUTPUT,
                            &output.name,
                            Action::SelectOutput(output.id.clone()),
                        )
                    })
                    .collect(),
            };
            content.footer = Some(link(text.control_sound_settings, Launch::SoundSettings));
        }
        Page::Media => {
            content.title = text.media_title;
            content.player = true;
        }
        Page::Screenshot => {
            content.title = text.control_screenshot;
            content.rows = vec![
                launch(glyph::REGION, text.control_region, Launch::Screenshot),
                launch(
                    glyph::SCREENSHOT,
                    text.control_full_screen,
                    Launch::CaptureScreen,
                ),
                launch(glyph::RECORD, text.control_record, Launch::RecordScreen),
            ];
            content.footer = Some(link(text.control_snipping_tool, Launch::SnippingTool));
        }
    }
    content
}

fn network_rows(look: &Look, text: &'static Strings) -> Vec<Row> {
    if look.readings.wifi == Some(false) {
        return vec![message(text.control_off)];
    }
    match &look.lists.wifi {
        None => vec![message(text.control_searching)],
        Some(WifiList::Unavailable) => vec![message(text.control_no_networks)],
        Some(WifiList::SavedOnly(networks)) => {
            // Room for the explanation and the way to the others.
            let mut rows: Vec<Row> = networks
                .iter()
                .take(SAVED_SHOWN)
                .map(|network| network_row(network, text))
                .collect();
            rows.push(Row {
                note: Some(text.control_location_off.to_owned()),
                ..message(text.control_saved_only)
            });
            // Windows' own list.
            rows.push(launch(
                glyph::ADD,
                text.control_other_networks,
                Launch::NetworkList,
            ));
            rows
        }
        Some(WifiList::Networks(networks)) if networks.is_empty() => {
            vec![message(text.control_no_networks)]
        }
        Some(WifiList::Networks(networks)) => networks
            .iter()
            .map(|network| network_row(network, text))
            .collect(),
    }
}

fn network_row(network: &WifiNetwork, text: &'static Strings) -> Row {
    let action = match &network.profile {
        _ if network.connected => None,
        Some(profile) => Some(Action::JoinWifi(profile.clone())),
        // A new network asks for its key in Windows' own list.
        None => Some(Action::Launch(Launch::NetworkList)),
    };
    Row {
        symbol: Some(network.signal.map_or(glyph::WIFI, glyph::signal)),
        label: network.name.clone(),
        note: network.connected.then(|| text.control_connected.to_owned()),
        checked: network.connected,
        action,
    }
}

/// How the screens share the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    ThisScreen,
    Duplicate,
    Extend,
    SecondScreen,
}

/// Where the page's parts go, in its own logical pixels: the page's top-left
/// corner is the grid's.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub height: f32,
    pub switch: Option<[f32; 4]>,
    /// A slider's track: left edge, width and the centre's height.
    pub slider: Option<[f32; 3]>,
    /// As many rows as fit.
    pub rows: Vec<[f32; 4]>,
    pub footer: Option<[f32; 4]>,
}

impl Layout {
    pub fn new(content: &Content) -> Self {
        let mut y = HEADER;
        let switch = content
            .switch
            .map(|_| [GRID - PAD - 40.0, (HEADER - 22.0) / 2.0, 40.0, 22.0]);
        let slider = content.slider.map(|_| {
            let track = [PAD + 24.0, GRID - 2.0 * PAD - 48.0, y + SLIDER / 2.0 - 4.0];
            y += SLIDER;
            track
        });
        if content.player {
            y += PLAYER;
        }
        let footer_room = content.footer.as_ref().map_or(0.0, |_| DIVIDER + ROW);
        let mut rows = Vec::new();
        for row in &content.rows {
            let height = if row.note.is_some() {
                ROW_WITH_NOTE
            } else {
                ROW
            };
            // The window is as tall as the grid; the footer must stay.
            if y + height + footer_room + BOTTOM > HEIGHT {
                break;
            }
            rows.push([6.0, y, GRID - 12.0, height]);
            y += height;
        }
        let footer = content.footer.as_ref().map(|_| {
            y += DIVIDER;
            let place = [6.0, y, GRID - 12.0, ROW];
            y += ROW;
            place
        });
        Self {
            height: y + BOTTOM,
            switch,
            slider,
            rows,
            footer,
        }
    }

    /// The whole page.
    pub fn area(&self) -> [f32; 4] {
        [0.0, 0.0, GRID, self.height]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Switch,
    Slider,
    Row(usize),
    Footer,
    Transport(Target),
    /// The page's own background.
    Nothing,
}

/// What a point (window pixels) is on; `None` outside the page.
pub fn hit(content: &Content, layout: &Layout, scale: f32, point: super::POINT) -> Option<Hit> {
    let on = |area: [f32; 4]| contains(pixels(scale, area), point);
    if !on(layout.area()) {
        return None;
    }
    if layout.switch.is_some_and(|[x, y, w, h]| {
        // A little larger than it looks.
        on([x - 8.0, y - 8.0, w + 16.0, h + 16.0])
    }) {
        return Some(Hit::Switch);
    }
    if layout
        .slider
        .is_some_and(|[left, width, y]| on([left - 8.0, y - 14.0, width + 16.0, 28.0]))
    {
        return Some(Hit::Slider);
    }
    if content.player
        && let Some((target, _)) = TRANSPORT
            .iter()
            .find(|(_, x)| on(around(layout.area(), GRID / 2.0 + x, TRANSPORT_Y, 40.0)))
    {
        return Some(Hit::Transport(*target));
    }
    if let Some(index) = layout.rows.iter().position(|row| on(*row)) {
        return Some(Hit::Row(index));
    }
    if layout.footer.is_some_and(on) {
        return Some(Hit::Footer);
    }
    Some(Hit::Nothing)
}

/// Draws everything but the page's background. Returns its slider.
pub fn draw(
    painter: &Painter<'_>,
    content: &Content,
    layout: &Layout,
    look: &Look,
) -> Result<Option<Slider>> {
    let page = layout.area();
    let width = GRID - 2.0 * PAD;
    let title_width = width - if layout.switch.is_some() { 52.0 } else { 0.0 };
    painter.text(page, PAD, 14.0, content.title, 15.0, true, 1.0, title_width)?;
    if let (Some(place), Some((_, on))) = (layout.switch, content.switch) {
        painter.switch(place, on)?;
    }
    let mut slider = None;
    if let (Some(track), Some((target, value))) = (layout.slider, content.slider) {
        let opacity = if value.is_some() { 1.0 } else { 0.35 };
        let (low, high) = if target == Target::BrightnessSlider {
            (glyph::BRIGHTNESS, glyph::BRIGHTNESS)
        } else {
            (glyph::VOLUME_LOW, glyph::VOLUME_HIGH)
        };
        painter.symbol(page, PAD + 8.0, track[2], low, 13.0, opacity)?;
        painter.symbol(page, GRID - PAD - 8.0, track[2], high, 18.0, opacity)?;
        slider = painter.slider(page, track, value)?;
    }
    if content.player {
        draw_player(painter, page, look)?;
    }
    for (row, place) in content.rows.iter().zip(&layout.rows) {
        draw_row(painter, row, *place)?;
    }
    if let (Some(row), Some(place)) = (&content.footer, layout.footer) {
        painter.line([PAD, place[1] - DIVIDER / 2.0, GRID - 2.0 * PAD, 1.0])?;
        draw_row(painter, row, place)?;
    }
    Ok(slider)
}

fn draw_row(painter: &Painter<'_>, row: &Row, place: [f32; 4]) -> Result<()> {
    let middle = place[3] / 2.0;
    let mut left = 10.0;
    if let Some(symbol) = row.symbol {
        let disc = around(place, 22.0, middle, 28.0);
        if row.checked {
            painter.circle(disc, painter.accent)?;
            painter.symbol_in(place, 22.0, middle, symbol, 14.0, super::WHITE)?;
        } else {
            painter.circle(disc, painter.wash())?;
            painter.symbol(place, 22.0, middle, symbol, 14.0, 1.0)?;
        }
        left = 46.0;
    }
    let width = place[2] - left - 10.0;
    let opacity = if row.action.is_some() || row.checked {
        1.0
    } else {
        0.7
    };
    match &row.note {
        Some(note) => {
            painter.text(place, left, 5.0, &row.label, 13.0, false, 1.0, width)?;
            painter.text(place, left, 23.0, note, 11.0, false, 0.65, width)
        }
        None => painter.text(
            place,
            left,
            middle - 9.0,
            &row.label,
            13.0,
            false,
            opacity,
            width,
        ),
    }
}

/// Minutes and seconds, with hours only when there are any.
fn clock(seconds: u64) -> String {
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn draw_player(painter: &Painter<'_>, page: [f32; 4], look: &Look) -> Result<()> {
    let track = look.track.as_ref();
    if let Some(track) = track
        && let Some(image) = media::artwork(track)
    {
        painter.image(image, [PAD, HEADER, ART, ART])?;
    }
    let text = crate::strings::text();
    let left = PAD + ART + 14.0;
    let width = GRID - left - PAD;
    let title = track.map_or(text.control_not_playing, |track| track.title.as_str());
    painter.text(page, left, HEADER + 18.0, title, 14.0, true, 1.0, width)?;
    if let Some(track) = track {
        painter.text(
            page,
            left,
            HEADER + 40.0,
            &track.artist,
            12.0,
            false,
            0.75,
            width,
        )?;
    }
    let bar = [PAD, PROGRESS_Y, GRID - 2.0 * PAD, 4.0];
    painter.rounded(
        pixels(painter.scale, bar),
        2.0 * painter.scale,
        painter.wash(),
    )?;
    if let Some((position, duration)) = look.position
        && duration > 0
    {
        let done = bar[2] * (position.min(duration) as f32 / duration as f32);
        painter.rounded(
            pixels(painter.scale, [bar[0], bar[1], done.max(4.0), bar[3]]),
            2.0 * painter.scale,
            painter.ink_color(),
        )?;
        painter.text(
            page,
            PAD,
            PROGRESS_Y + 8.0,
            &clock(position),
            10.0,
            false,
            0.7,
            70.0,
        )?;
        painter.text_right(
            page,
            GRID - PAD,
            PROGRESS_Y + 8.0,
            &clock(duration),
            10.0,
            0.7,
        )?;
    }
    for (target, x) in TRANSPORT {
        let (symbol, enabled) = super::transport(target, track);
        painter.symbol(
            page,
            GRID / 2.0 + x,
            TRANSPORT_Y,
            symbol,
            if target == Target::PlayPause {
                30.0
            } else {
                22.0
            },
            if enabled { 1.0 } else { 0.35 },
        )?;
    }
    Ok(())
}
