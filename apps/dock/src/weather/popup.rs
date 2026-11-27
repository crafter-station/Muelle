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
use data::Data;
use layout::Layout;
use muelle_config::Language;
use muelle_core::frame::DockEdge;
use std::sync::OnceLock;
use windows::{
    UI::Composition::Desktop::DesktopWindowTarget,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{
                GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_SHIFT,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{Interface, w},
};

pub const CLOSE: isize = -1;
pub const NEXT: isize = -2;
pub const PREVIOUS: isize = -3;
pub const ACTIVATE: isize = -4;

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
    focus: Option<usize>,
    pressed: Option<usize>,
    previous: HWND,
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
            focus: None,
            pressed: None,
            previous: HWND::default(),
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
            self.pressed = None;
            let view = view::build(gfx, context, layout, data)?;
            self.target.SetRoot(&view.root)?;
            self.view = view;
            self.caption(data)?;
            if let Some(index) = self.focus {
                self.view.focus[index].SetIsVisible(true)?;
            }
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
        let t = crate::weather::text::text(self.context.language);
        unsafe {
            SetWindowTextW(
                self.hwnd,
                &windows::core::HSTRING::from(format!(
                    "{} · {} · {} · {}",
                    t.title, data.location, data.condition, data.degrees
                )),
            )?;
        }
        Ok(())
    }
    pub fn action(&mut self, event: isize) -> Result<Option<usize>> {
        match event {
            NEXT | PREVIOUS => {
                let next = match (self.focus, event) {
                    (Some(index), NEXT) => (index + 1) % 3,
                    (Some(index), _) => (index + 2) % 3,
                    (None, NEXT) => 0,
                    _ => 2,
                };
                if let Some(index) = self.focus {
                    self.view.focus[index].SetIsVisible(false)?;
                }
                self.focus = Some(next);
                self.view.focus[next].SetIsVisible(true)?;
                Ok(None)
            }
            ACTIVATE => Ok(self.focus),
            CLOSE => Ok(Some(0)),
            down if down <= -16 => {
                let point = (-16 - down) as u32;
                self.pressed = self.layout.hit(POINT {
                    x: (point & 0xffff) as i32,
                    y: (point >> 16) as i32,
                });
                Ok(None)
            }
            click if click >= 0 => {
                let hit = self.layout.hit(POINT {
                    x: (click as u32 & 0xffff) as i32,
                    y: ((click as u32 >> 16) & 0xffff) as i32,
                });
                let pressed = self.pressed.take();
                Ok(hit.filter(|hit| pressed == Some(*hit)))
            }
            _ => Ok(None),
        }
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
            match w.0 {
                0x1b => post(hwnd, CLOSE),
                0x09 => post(
                    hwnd,
                    if unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0 {
                        PREVIOUS
                    } else {
                        NEXT
                    },
                ),
                0x25 | 0x26 => post(hwnd, PREVIOUS),
                0x27 | 0x28 => post(hwnd, NEXT),
                0x0d | 0x20 if l.0 & (1 << 30) == 0 => post(hwnd, ACTIVATE),
                _ => {}
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
        forecast.hours = (1..24)
            .map(|i| muelle_weather::Hour {
                time: forecast.current.time + i * 3600,
                temperature_c: 15.0 + (i % 7) as f32,
                condition: muelle_weather::Condition::PartlyCloudy,
                precipitation_percent: Some((i * 3) as u8),
                is_day: Some(i < 8),
            })
            .collect();
        forecast.days = (0..5)
            .map(|i| muelle_weather::Day {
                time: start + i * 86400,
                low_c: 10.0 + i as f32,
                high_c: 19.0 + i as f32,
                condition: muelle_weather::Condition::PartlyCloudy,
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
                        let root = popup.target.Root()?;
                        assert_eq!(backdrops(&popup.view.root)?, usize::from(glass));
                        assert_ne!(unsafe { GetForegroundWindow() }, hwnd);
                        assert_ne!(unsafe { GetCapture() }, hwnd);
                        assert_eq!(popup.action(ACTIVATE)?, None);
                        let point = POINT {
                            x: (366.0 * popup.layout.scale) as i32,
                            y: (30.0 * popup.layout.scale) as i32,
                        };
                        let click = (((point.y as u32) << 16) | point.x as u32) as isize;
                        assert_eq!(
                            popup.action(click)?,
                            None,
                            "a release alone must not activate a button"
                        );
                        popup.action(-16 - click)?;
                        assert_eq!(popup.action(click)?, Some(0));
                        popup.action(-16 - click)?;
                        assert_eq!(
                            popup.action(0)?,
                            None,
                            "release in another control/empty space cancels the press"
                        );
                        assert_eq!(
                            popup.action(click)?,
                            None,
                            "a cancelled press must not remain armed"
                        );
                        popup.action(NEXT)?;
                        assert_eq!(popup.action(ACTIVATE)?, Some(0));
                        popup.action(PREVIOUS)?;
                        assert_eq!(popup.action(ACTIVATE)?, Some(2));
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
                        assert!(popup.view.focus[2].IsVisible()?);
                        assert_eq!(popup.action(CLOSE)?, Some(0));
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
