//! An explicitly opened, event-driven weather panel. No worker or timer here.
#[path = "popup/data.rs"]
pub mod data;
#[path = "popup/layout.rs"]
mod layout;
#[path = "popup/text.rs"]
pub mod text;
#[path = "popup/view.rs"]
mod view;
use crate::{appearance::Look, gfx::Gfx};
use anyhow::Result;
use data::{Data, Selection};
use layout::{Hit, Layout};
use muelle_config::Language;
use muelle_core::frame::DockEdge;
use std::sync::OnceLock;
use windows::{
    UI::Composition::Desktop::DesktopWindowTarget,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{Interface, w},
};

pub const CLOSE: isize = -1;
const NEXT_DAY: isize = -2;
const PREVIOUS_DAY: isize = -3;
const NEXT_HOURS: isize = -4;
const PREVIOUS_HOURS: isize = -5;
const TODAY: isize = -6;
pub enum Action {
    Close,
    Select(Selection),
}

#[derive(Clone, Copy, PartialEq)]
pub struct Context {
    pub anchor: RECT,
    pub screen: RECT,
    pub edge: DockEdge,
    pub scale: f32,
    pub look: Look,
    pub language: Language,
    pub revision: u64,
    pub hour: i64,
    pub has_data: bool,
}
impl Context {
    fn same_surface(self, other: Self) -> bool {
        self.screen == other.screen
            && self.scale == other.scale
            && self.look == other.look
            && self.language == other.language
            && self.revision == other.revision
            && self.hour == other.hour
            && self.has_data == other.has_data
    }
}
pub struct Popup {
    hwnd: HWND,
    target: DesktopWindowTarget,
    context: Context,
    layout: Layout,
    view: view::View,
    previous: HWND,
    selection: Selection,
    days: Vec<i64>,
    hours: Vec<Option<i64>>,
    previous_hours: bool,
    next_hours: bool,
    pressed: Option<Hit>,
}
impl Popup {
    /// Construct hidden. Focus/capture are acquired only for a user's explicit click.
    pub fn new(gfx: &Gfx, context: Context, data: &Data) -> Result<Self> {
        let layout = Layout::new(
            context.anchor,
            context.screen,
            context.edge,
            context.scale,
            context.has_data,
        )
        .ok_or_else(|| anyhow::anyhow!("weather popup cannot fit monitor"))?;
        let view = view::build(gfx, context, layout, data)?;
        let hwnd = create(layout.bounds)?;
        let target = unsafe {
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)
        };
        let target = match target {
            Ok(target) => target,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(e.into());
            }
        };
        let popup = Self {
            hwnd,
            target,
            context,
            layout,
            view,
            previous: HWND::default(),
            selection: data.selection,
            days: data.days.iter().map(|d| d.time).collect(),
            hours: data.hours.iter().map(|h| h.time).collect(),
            previous_hours: data.previous_hours,
            next_hours: data.next_hours,
            pressed: None,
        };
        popup.target.SetRoot(&popup.view.root)?;
        popup.caption(data)?;
        Ok(popup)
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    pub fn show(&mut self) -> bool {
        unsafe {
            self.previous = GetForegroundWindow();
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = SetForegroundWindow(self.hwnd);
            if GetForegroundWindow() != self.hwnd {
                return false;
            }
            let _ = SetFocus(Some(self.hwnd));
            SetCapture(self.hwnd);
        }
        true
    }
    pub fn needs_data(&self, context: Context) -> bool {
        !self.context.same_surface(context)
    }
    pub fn same_display(&self, context: Context) -> bool {
        self.context.screen == context.screen && self.context.edge == context.edge
    }
    pub fn update(&mut self, gfx: &Gfx, context: Context, data: Option<&Data>) -> Result<()> {
        let layout = Layout::new(
            context.anchor,
            context.screen,
            context.edge,
            context.scale,
            context.has_data,
        )
        .ok_or_else(|| anyhow::anyhow!("weather popup cannot fit monitor"))?;
        if let Some(data) = data {
            self.context = context;
            let view = view::build(gfx, context, layout, data)?;
            self.target.SetRoot(&view.root)?;
            self.view = view;
            self.caption(data)?;
            self.selection = data.selection;
            self.days = data.days.iter().map(|d| d.time).collect();
            self.hours = data.hours.iter().map(|h| h.time).collect();
            self.previous_hours = data.previous_hours;
            self.next_hours = data.next_hours;
            self.pressed = None;
        }
        if self.layout.bounds != layout.bounds {
            let r = layout.bounds;
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE,
                )?;
            }
        }
        self.context = context;
        self.layout = layout;
        Ok(())
    }
    fn caption(&self, data: &Data) -> Result<()> {
        unsafe {
            SetWindowTextW(
                self.hwnd,
                &windows::core::HSTRING::from(format!(
                    "{} · {} · {} · {}",
                    data.title, data.location, data.condition, data.degrees
                )),
            )?;
        }
        Ok(())
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn action(&mut self, event: isize) -> Option<Action> {
        if event == CLOSE {
            return Some(Action::Close);
        }
        if !self.context.has_data {
            return None;
        }
        let hit = match event {
            NEXT_DAY | PREVIOUS_DAY => {
                let current = self
                    .days
                    .iter()
                    .position(|t| Some(*t) == self.selection.day)
                    .unwrap_or(0);
                let next = if event == NEXT_DAY {
                    (current + 1).min(self.days.len().saturating_sub(1))
                } else {
                    current.saturating_sub(1)
                };
                Some(Hit::Day(next))
            }
            NEXT_HOURS => Some(Hit::NextHours),
            PREVIOUS_HOURS => Some(Hit::PreviousHours),
            TODAY => Some(Hit::Day(0)),
            down if down <= -16 => {
                self.pressed = self.layout.hit(unpack(-16 - down));
                return None;
            }
            click if click >= 0 => {
                let hit = self.layout.hit(unpack(click));
                let pressed = self.pressed.take();
                hit.filter(|h| Some(*h) == pressed)
            }
            _ => None,
        }?;
        let next = match hit {
            Hit::Day(index) => Selection {
                day: Some(*self.days.get(index)?),
                page: 0,
                hour: None,
            },
            Hit::Hour(index) => Selection {
                hour: *self.hours.get(index)?,
                ..self.selection
            },
            Hit::PreviousHours if self.previous_hours => Selection {
                page: self.selection.page.saturating_sub(1),
                hour: None,
                ..self.selection
            },
            Hit::NextHours if self.next_hours => Selection {
                page: self.selection.page + 1,
                hour: None,
                ..self.selection
            },
            _ => return None,
        };
        (next != self.selection).then_some(Action::Select(next))
    }
    pub fn refresh_backdrops(&self, gfx: &Gfx) -> Result<()> {
        gfx.refresh_backdrops(&self.view.root)?;
        Ok(())
    }
}
impl Drop for Popup {
    fn drop(&mut self) {
        // Disconnect this window's Composition target explicitly. The shared
        // compositor survives every popup, but its old targets must not.
        let _ = self
            .target
            .SetRoot(None::<&windows::UI::Composition::Visual>);
        let _ = self.target.Close();
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
            // Escape restores the previous app; an external focus change wins.
            if GetForegroundWindow() == self.hwnd {
                let previous = if IsWindow(Some(self.previous)).as_bool()
                    && IsWindowVisible(self.previous).as_bool()
                    && !IsIconic(self.previous).as_bool()
                {
                    self.previous
                } else {
                    GetShellWindow()
                };
                let _ = SetForegroundWindow(previous);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
fn create(bounds: RECT) -> Result<HWND> {
    static CLASS: OnceLock<()> = OnceLock::new();
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("MuelleWeatherPopup");
        CLASS.get_or_init(|| {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
        });
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("Weather"),
            WS_POPUP,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        crate::menu::apply_popup_frame(hwnd);
        Ok(hwnd)
    }
}
fn unpack(value: isize) -> POINT {
    POINT {
        x: (value as u32 & 0xffff) as i16 as i32,
        y: ((value as u32 >> 16) & 0xffff) as i16 as i32,
    }
}
fn post(hwnd: HWND, action: isize) {
    crate::app::weather_popup_signal(hwnd, action);
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let point = POINT {
        x: (l.0 as u32 & 0xffff) as i16 as i32,
        y: ((l.0 as u32 >> 16) & 0xffff) as i16 as i32,
    };
    match msg {
        WM_CLOSE => {
            post(hwnd, CLOSE);
            return LRESULT(0);
        }
        WM_ACTIVATE if w.0 & 0xffff == WA_INACTIVE as usize => post(hwnd, CLOSE),
        WM_CAPTURECHANGED if l.0 != hwnd.0 as isize => post(hwnd, CLOSE),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
            let mut rect = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut rect);
            }
            if point.x < 0 || point.y < 0 || point.x >= rect.right || point.y >= rect.bottom {
                post(hwnd, CLOSE);
            } else if msg == WM_LBUTTONDOWN {
                post(hwnd, -16 - l.0 as u32 as isize);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            if point.x >= 0 && point.y >= 0 {
                post(hwnd, l.0 as u32 as isize);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let action = match w.0 {
                0x1b => Some(CLOSE),
                0x28 => Some(NEXT_DAY),
                0x26 => Some(PREVIOUS_DAY),
                0x27 => Some(NEXT_HOURS),
                0x25 => Some(PREVIOUS_HOURS),
                0x24 => Some(TODAY),
                _ => None,
            };
            if let Some(action) = action {
                post(hwnd, action);
            }
            return LRESULT(0);
        }
        WM_CHAR => return LRESULT(0),
        WM_SETCURSOR => {
            unsafe {
                if let Ok(cursor) = LoadCursorW(None, IDC_ARROW) {
                    SetCursor(Some(cursor));
                }
            }
            return LRESULT(1);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Material;
    use windows::{
        UI::Composition::{CompositionBackdropBrush, ContainerVisual, SpriteVisual},
        Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        },
    };
    fn backdrops(root: &ContainerVisual) -> Result<usize> {
        let mut count = 0;
        if let Ok(sprite) = root.cast::<SpriteVisual>()
            && let Ok(brush) = sprite.Brush()
            && brush.cast::<CompositionBackdropBrush>().is_ok()
        {
            count += 1;
        }
        for visual in root.Children()? {
            if let Ok(child) = visual.cast::<ContainerVisual>() {
                count += backdrops(&child)?;
            }
        }
        Ok(count)
    }
    #[test]
    #[ignore = "Hidden owned popup only: no focus, capture, location, network or live Dock changes"]
    fn native_forecast_panel_retains_on_moves_tracks_material_and_releases_resources() -> Result<()>
    {
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut forecast = muelle_weather::decode(include_bytes!(
            "../../../../crates/muelle-weather/tests/forecast.json"
        ))?;
        let start = forecast.days[0].time;
        forecast.hours = (1..120)
            .map(|i| muelle_weather::Hour {
                time: forecast.current.time + i * 3600,
                temperature_c: 15.0 + (i % 7) as f32,
                feels_like_c: Some(14.0),
                wind_kmh: Some(9.0),
                condition: muelle_weather::Condition::PartlyCloudy,
                precipitation_percent: Some(((i * 3) % 100) as u8),
                is_day: Some(i < 8),
            })
            .collect();
        forecast.days = (0..5)
            .map(|i| muelle_weather::Day {
                time: start + i * 86400,
                low_c: 10.0 + i as f32,
                high_c: 19.0 + i as f32,
                condition: muelle_weather::Condition::PartlyCloudy,
                wind_max_kmh: Some(18.0),
                precipitation_percent: Some(30),
                sunrise: Some(start + i * 86400 + 6 * 3600),
                sunset: Some(start + i * 86400 + 18 * 3600),
            })
            .collect();
        let data = Data::new(
            Some(&forecast),
            Language::Es,
            forecast.current.time,
            data::Status {
                has_location: true,
                ..Default::default()
            },
        );
        assert_eq!(data.hours.len(), 6);
        assert_eq!(data.days.len(), 5);
        for material in Material::ALL {
            for light in [false, true] {
                for glass in [false, true] {
                    for scale in [1.0, 1.5] {
                        let context = Context {
                            anchor: RECT {
                                left: -32000,
                                top: -31000,
                                right: -31780,
                                bottom: -30952,
                            },
                            screen: RECT {
                                left: -33000,
                                top: -33000,
                                right: -30000,
                                bottom: -30000,
                            },
                            edge: DockEdge::Bottom,
                            scale,
                            look: Look {
                                material,
                                light,
                                glass,
                            },
                            language: Language::Es,
                            revision: 1,
                            hour: 0,
                            has_data: true,
                        };
                        let mut popup = Popup::new(&gfx, context, &data)?;
                        let hwnd = popup.hwnd();
                        assert_eq!(backdrops(&popup.view.root)?, usize::from(glass));
                        assert_ne!(unsafe { GetForegroundWindow() }, hwnd);
                        assert_ne!(unsafe { GetCapture() }, hwnd);
                        assert!(popup.action(9999).is_none());
                        assert!(popup.action(0).is_none());
                        assert!(matches!(
                            popup.action(NEXT_DAY),
                            Some(Action::Select(Selection {
                                day: Some(_),
                                page: 0,
                                hour: None
                            }))
                        ));
                        assert!(popup.action(PREVIOUS_DAY).is_none());
                        let click = (((383.0 * popup.layout.scale) as u32) << 16)
                            | (40.0 * popup.layout.scale) as u32;
                        assert!(
                            popup.action(click as isize).is_none(),
                            "a release alone cannot select a day"
                        );
                        popup.action(-16 - click as isize);
                        let Some(Action::Select(selection)) = popup.action(click as isize) else {
                            anyhow::bail!("matching day press/release must select it at every DPI");
                        };
                        assert_eq!(selection.day, Some(forecast.days[1].time));
                        let future = Data::for_selection(
                            Some(&forecast),
                            Language::Es,
                            forecast.current.time,
                            data::Status {
                                has_location: true,
                                ..Default::default()
                            },
                            selection,
                        );
                        popup.update(&gfx, context, Some(&future))?;
                        assert_eq!(popup.selection(), future.selection);
                        assert!(future.future && future.hours.len() == 6);
                        assert!(matches!(
                            popup.action(NEXT_HOURS),
                            Some(Action::Select(Selection { page: 1, .. }))
                        ));
                        let hour_click = (((241.0 * popup.layout.scale) as u32) << 16)
                            | (108.0 * popup.layout.scale) as u32;
                        assert!(popup.action(hour_click as isize).is_none());
                        popup.action(-16 - hour_click as isize);
                        let Some(Action::Select(hour_selection)) =
                            popup.action(hour_click as isize)
                        else {
                            anyhow::bail!(
                                "matching hourly press/release must select its own timestamp"
                            );
                        };
                        assert_eq!(hour_selection.hour, future.hours[1].time);
                        let hourly = Data::for_selection(
                            Some(&forecast),
                            Language::Es,
                            forecast.current.time,
                            data::Status {
                                has_location: true,
                                ..Default::default()
                            },
                            hour_selection,
                        );
                        popup.update(&gfx, context, Some(&hourly))?;
                        assert_eq!(popup.selection().hour, hourly.hours[1].time);
                        assert!(hourly.hours[1].selected);
                        assert!(popup.action(hour_click as isize).is_none());
                        let root = popup.target.Root()?;

                        assert!(popup.action(9999).is_none());
                        let context = Context {
                            anchor: RECT {
                                left: context.anchor.left + 20,
                                right: context.anchor.right + 20,
                                ..context.anchor
                            },
                            ..context
                        };
                        assert!(!popup.needs_data(context));
                        popup.update(&gfx, context, None)?;
                        assert!(
                            popup.target.Root()? == root,
                            "moving preserves text and art"
                        );
                        let changed = Context {
                            look: Look {
                                glass: !glass,
                                ..context.look
                            },
                            ..context
                        };
                        assert!(popup.needs_data(changed));
                        popup.update(&gfx, changed, Some(&data))?;
                        assert_eq!(backdrops(&popup.view.root)?, usize::from(!glass));
                        assert_eq!(popup.hwnd(), hwnd);
                        assert!(matches!(popup.action(CLOSE), Some(Action::Close)));
                        drop(popup);
                        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "popup/performance.rs"]
mod performance;
