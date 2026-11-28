use super::Placement;
use crate::{
    appearance::Look,
    gfx::{Gfx, Ink, LabelBrush, WeatherGlyph},
};
use anyhow::Result;
use std::sync::OnceLock;
use windows::{
    UI::{
        Color,
        Composition::{CompositionSurfaceBrush, ContainerVisual, Desktop::DesktopWindowTarget},
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
                HWND_TOPMOST, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, RegisterClassW,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetCursor,
                SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
                WM_SETCURSOR, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{HSTRING, Interface, w},
};
use windows_numerics::{Vector2, Vector3};
const WM_MOUSELEAVE: u32 = 0x02A3; // Not exported by this Windows metadata version.

#[derive(Clone, Copy, PartialEq)]
pub struct Key {
    pub placement: Placement,
    pub screen: RECT,
    pub look: Look,
    pub scale: f32,
    pub revision: u64,
    pub language: muelle_config::Language,
}

impl Key {
    pub fn visible_bounds(self) -> Option<RECT> {
        let r = self.placement.bounds;
        let visible = RECT {
            left: r.left.max(self.screen.left),
            top: r.top.max(self.screen.top),
            right: r.right.min(self.screen.right),
            bottom: r.bottom.min(self.screen.bottom),
        };
        (visible.left < visible.right && visible.top < visible.bottom).then_some(visible)
    }
    pub fn same_surface(self, other: Self) -> bool {
        let a = self.placement.bounds;
        let b = other.placement.bounds;
        a.right - a.left == b.right - b.left
            && a.bottom - a.top == b.bottom - b.top
            && self.placement.compact == other.placement.compact
            && self.scale == other.scale
            && self.revision == other.revision
            && self.language == other.language
    }
}

pub struct Model {
    pub degrees: String,
    pub title: String,
    pub subtitle: String,
    pub glyph: WeatherGlyph,
}

pub struct Card {
    hwnd: HWND,
    target: DesktopWindowTarget,
    pub key: Key,
}

impl Card {
    pub fn new(gfx: &Gfx, key: Key, model: &Model) -> Result<Self> {
        let root = surface(gfx, key, model)?;
        let visible = key
            .visible_bounds()
            .ok_or_else(|| anyhow::anyhow!("weather outside monitor"))?;
        let hwnd = create(visible)?;
        let target = unsafe {
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(error.into());
            }
        };
        let view = Self { hwnd, target, key };
        view.target.SetRoot(&root)?;
        view.crop(key)?;
        view.caption(model)?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(view)
    }

    pub fn update(&mut self, gfx: &Gfx, key: Key, model: &Model) -> Result<()> {
        if !self.key.same_surface(key) {
            let previous = self.target.Root()?.cast()?;
            self.target.SetRoot(&surface(gfx, key, model)?)?;
            super::close_tree(&previous);
            self.crop(key)?;
        }
        if self.key.revision != key.revision || self.key.language != key.language {
            self.caption(model)?;
        }
        self.reposition(key)
    }

    /// Only translate the retained window during the Dock's existing animation.
    pub fn reposition(&mut self, key: Key) -> Result<()> {
        if self.key.placement != key.placement || self.key.screen != key.screen {
            let r = key
                .visible_bounds()
                .ok_or_else(|| anyhow::anyhow!("weather outside monitor"))?;
            // Crop the actual HWND too: no transparent input area or pixels
            // may slide onto a different monitor across this screen's edge.
            self.crop(key)?;
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
        self.key = key;
        Ok(())
    }
    fn crop(&self, key: Key) -> Result<()> {
        let visible = key
            .visible_bounds()
            .ok_or_else(|| anyhow::anyhow!("weather outside monitor"))?;
        let full = key.placement.bounds;
        self.target.Root()?.SetOffset(Vector3::new(
            (full.left - visible.left) as f32,
            (full.top - visible.top) as f32,
            0.0,
        ))?;
        Ok(())
    }
    fn caption(&self, model: &Model) -> Result<()> {
        unsafe {
            SetWindowTextW(
                self.hwnd,
                &HSTRING::from(format!(
                    "{} · {} · {}",
                    model.degrees, model.title, model.subtitle
                )),
            )?;
        }
        Ok(())
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    pub fn track_after_menu(&self, inside: bool) -> bool {
        // A modal menu held capture. Re-arm leave tracking once after it closes,
        // so a stationary pointer does not either hide or pin the Dock forever.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            if inside {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut tracking).is_ok() {
                    SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 1);
                    return true;
                }
            }
        }
        false
    }
    pub fn raise(&self) {
        // The Dock's larger invisible magnification window must not sit over
        // this card's input. Only called with existing Dock placement work.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }
    pub fn refresh_backdrops(&self, gfx: &Gfx) -> Result<()> {
        if let Ok(root) = self.target.Root() {
            gfx.refresh_backdrops(&root.cast()?)?;
        }
        Ok(())
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        let previous = self
            .target
            .Root()
            .and_then(|v| v.cast::<ContainerVisual>())
            .ok();
        let _ = self
            .target
            .SetRoot(None::<&windows::UI::Composition::Visual>);
        let _ = self.target.Close();
        if let Some(root) = previous {
            super::close_tree(&root);
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn add(
    gfx: &Gfx,
    root: &ContainerVisual,
    brush: &CompositionSurfaceBrush,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Result<()> {
    let sprite = gfx.compositor.CreateSpriteVisual()?;
    sprite.SetBrush(brush)?;
    sprite.SetSize(Vector2::new(w, h))?;
    sprite.SetOffset(Vector3::new(x, y, 0.0))?;
    root.Children()?.InsertAtTop(&sprite)?;
    Ok(())
}
fn label(
    gfx: &Gfx,
    root: &ContainerVisual,
    brush: LabelBrush,
    x: f32,
    y: f32,
    opacity: f32,
) -> Result<()> {
    let sprite = gfx.compositor.CreateSpriteVisual()?;
    sprite.SetBrush(&brush.brush)?;
    sprite.SetSize(Vector2::new(brush.width, brush.height))?;
    sprite.SetOffset(Vector3::new(x, y, 0.0))?;
    sprite.SetOpacity(opacity)?;
    root.Children()?.InsertAtTop(&sprite)?;
    Ok(())
}

fn surface(gfx: &Gfx, key: Key, model: &Model) -> Result<ContainerVisual> {
    let r = key.placement.bounds;
    let (width, height) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    let root = gfx.compositor.CreateContainerVisual()?;
    root.SetSize(Vector2::new(width, height))?;
    let radius = (height.min(width) * 0.28).min(18.0 * key.scale);
    let round = gfx.compositor.CreateRoundedRectangleGeometry()?;
    round.SetSize(Vector2::new(width, height))?;
    round.SetCornerRadius(Vector2::new(radius, radius))?;
    root.SetClip(&gfx.compositor.CreateGeometricClipWithGeometry(&round)?)?;
    // A deliberate opaque blue weather tile inside the Dock's chosen glass:
    // one static gradient, no extra backdrop, capture, shaders or animation.
    let tint = gfx.compositor.CreateLinearGradientBrush()?;
    tint.SetStartPoint(Vector2::new(0.0, 0.0))?;
    tint.SetEndPoint(Vector2::new(1.0, 1.0))?;
    let night = matches!(model.glyph, WeatherGlyph::Moon | WeatherGlyph::NightCloud);
    let colors = if night {
        [(0.0, (54, 105, 174)), (1.0, (28, 53, 112))]
    } else {
        [(0.0, (81, 174, 233)), (1.0, (39, 112, 211))]
    };
    for (at, (red, green, blue)) in colors {
        tint.ColorStops()?
            .Append(&gfx.compositor.CreateColorGradientStopWithOffsetAndColor(
                at,
                Color {
                    A: 255,
                    R: red,
                    G: green,
                    B: blue,
                },
            )?)?;
    }
    let sky = gfx.compositor.CreateSpriteVisual()?;
    sky.SetSize(Vector2::new(width, height))?;
    sky.SetBrush(&tint)?;
    root.Children()?.InsertAtTop(&sky)?;
    if key.placement.compact {
        // A side Dock remains narrow: upright degrees above an illustration,
        // rather than rotated text or an external floating weather window.
        let u = width / 48.0;
        let degrees = gfx.text_brush_fitted(
            &model.degrees,
            u,
            24.0,
            false,
            Ink::Light,
            (width - 8.0 * u).max(1.0),
        )?;
        let x = ((width - degrees.width) / 2.0).max(0.0);
        label(gfx, &root, degrees, x, 6.0 * u, 1.0)?;
        let side = width * 1.1;
        let icon = gfx.weather_icon(model.glyph, side / 40.0)?;
        add(
            gfx,
            &root,
            &icon,
            (width - side) / 2.0,
            (height - side - 3.0 * u).max(0.0),
            side,
            side,
        )?;
    } else {
        let u = height / 48.0;
        let degrees_x = width - 104.0 * u;
        let text_width = (degrees_x - 18.0 * u).max(1.0);
        label(
            gfx,
            &root,
            gfx.text_brush_fitted(&model.title, u, 11.5, true, Ink::Light, text_width)?,
            12.0 * u,
            7.0 * u,
            1.0,
        )?;
        label(
            gfx,
            &root,
            gfx.text_brush_fitted(&model.subtitle, u, 10.5, false, Ink::Light, text_width)?,
            12.0 * u,
            27.0 * u,
            0.82,
        )?;
        let font = match model.degrees.chars().count() {
            5.. => 23.0,
            4 => 29.0,
            _ => 36.0,
        };
        let degrees =
            gfx.text_brush_fitted(&model.degrees, u, font, false, Ink::Light, 63.0 * u)?;
        let y = ((height - degrees.height) / 2.0).max(0.0);
        label(gfx, &root, degrees, degrees_x, y, 1.0)?;
        // Art can bleed into the rounded right edge, as in the reference.
        // It is clipped by the tile, never by the neighboring app's bounds.
        let side = height * 1.22;
        let icon = gfx.weather_icon(model.glyph, side / 40.0)?;
        add(
            gfx,
            &root,
            &icon,
            width - side * 0.82,
            (height - side) * 0.5,
            side,
            side,
        )?;
    }
    Ok(root)
}

fn create(r: RECT) -> Result<HWND> {
    static CLASS: OnceLock<()> = OnceLock::new();
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("MuelleWeather");
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
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        crate::menu::apply_popup_frame(hwnd);
        Ok(hwnd)
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => unsafe {
            if GetWindowLongPtrW(hwnd, GWLP_USERDATA) & 1 == 0 {
                let mut tracked = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut tracked).is_ok() {
                    SetWindowLongPtrW(
                        hwnd,
                        GWLP_USERDATA,
                        GetWindowLongPtrW(hwnd, GWLP_USERDATA) | 1,
                    );
                    crate::app::weather_signal(hwnd, 1);
                }
            }
        },
        WM_MOUSELEAVE => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            crate::app::weather_signal(hwnd, 0);
        },
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => unsafe {
            let bit = if msg == WM_LBUTTONDOWN { 2 } else { 4 };
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) | bit,
            );
        },
        WM_LBUTTONUP | WM_RBUTTONUP => unsafe {
            let bit = if msg == WM_LBUTTONUP { 2 } else { 4 };
            let flags = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, flags & !bit);
            // An outside click can dismiss the popup on button-down. Its
            // later release over the card must not immediately reopen it.
            if flags & bit != 0 {
                crate::app::weather_signal(hwnd, if bit == 2 { 2 } else { 3 });
            }
        },
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
        UI::Composition::{CompositionBackdropBrush, SpriteVisual},
        Win32::{
            Foundation::RECT,
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, IsWindow},
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
    #[ignore = "Offscreen weather cards only; no location, network, real Dock or focus changes"]
    fn native_weather_cards_retain_windows_clip_the_sky_and_bound_materials() -> Result<()> {
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for material in Material::ALL {
                for light in [false, true] {
                    for glass in [false, true] {
                        for compact in [false, true] {
                            let width = if compact { 48.0 } else { 220.8 };
                            let height = if compact { 105.6 } else { 48.0 };
                            let bounds = RECT {
                                left: -32000,
                                top: -32000,
                                right: -32000 + (width * scale) as i32,
                                bottom: -32000 + (height * scale) as i32,
                            };
                            let key = Key {
                                placement: Placement { bounds, compact },
                                screen: RECT {
                                    left: -33000,
                                    top: -33000,
                                    right: -30000,
                                    bottom: -30000,
                                },
                                look: Look {
                                    material,
                                    light,
                                    glass,
                                },
                                scale,
                                revision: 0,
                                language: muelle_config::Language::Es,
                            };
                            let model = Model {
                                degrees: "-100°".into(),
                                title: "Parcialmente nublado".into(),
                                subtitle: "Ubicación actual".into(),
                                glyph: WeatherGlyph::PartlyCloudy,
                            };
                            let mut card = Card::new(&gfx, key, &model)?;
                            let hwnd = card.hwnd();
                            assert_ne!(
                                unsafe { GetForegroundWindow() },
                                hwnd,
                                "the non-activating card must not take focus"
                            );
                            let root: ContainerVisual = card.target.Root()?.cast()?;
                            assert_eq!(backdrops(&root)?, 0, "the blue tile adds no backdrop");
                            assert!(root.Clip().is_ok());
                            // The weather illustration deliberately bleeds past
                            // the right edge; the root's rounded clip bounds it.
                            let retained = root.clone();
                            let moved = Key {
                                placement: Placement {
                                    bounds: RECT {
                                        left: bounds.left + 1,
                                        right: bounds.right + 1,
                                        ..bounds
                                    },
                                    compact,
                                },
                                ..key
                            };
                            card.update(&gfx, moved, &model)?;
                            assert!(
                                card.target.Root()?
                                    == retained.cast::<windows::UI::Composition::Visual>()?,
                                "moving the Dock must not rasterize weather text or art"
                            );
                            let clipped = Key {
                                screen: RECT {
                                    left: bounds.left + 10,
                                    ..key.screen
                                },
                                ..key
                            };
                            card.update(&gfx, clipped, &model)?;
                            let mut native_bounds = RECT::default();
                            unsafe { GetWindowRect(hwnd, &mut native_bounds)? };
                            assert_eq!(
                                Some(native_bounds),
                                clipped.visible_bounds(),
                                "the input window must not leak onto an adjacent monitor"
                            );
                            assert_eq!(card.target.Root()?.Offset()?.X, -10.0);
                            assert!(
                                card.target.Root()?
                                    == retained.cast::<windows::UI::Composition::Visual>()?
                            );
                            card.update(
                                &gfx,
                                Key { revision: 1, ..key },
                                &Model {
                                    degrees: "17°".into(),
                                    title: "Clear".into(),
                                    subtitle: "Current location".into(),
                                    glyph: WeatherGlyph::Sun,
                                },
                            )?;
                            assert_eq!(
                                card.hwnd(),
                                hwnd,
                                "a data update must keep the input window"
                            );
                            assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());
                            assert_ne!(
                                unsafe { GetForegroundWindow() },
                                hwnd,
                                "updating or moving the card must not take focus"
                            );
                            drop(card);
                            assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
