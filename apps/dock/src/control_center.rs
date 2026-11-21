//! macOS-style Control Center popup panel for Muelle.
//!
//! Provides interactive quick controls: display brightness slider, master sound
//! volume slider with mute, dark/light theme toggle, system shortcuts, and
//! hardware status.

use anyhow::Result;
use muelle_core::control_center::{
    ControlCenterAction, ControlCenterLayout, PANEL_RADIUS, SliderState, TILE_RADIUS,
};
use muelle_shell::{
    control_center::{BrightnessControl, Personalization, Shortcuts},
    status::AudioWatch,
};
use std::{cell::RefCell, time::Instant};
use windows::{
    UI::{
        Color,
        Composition::{Desktop::DesktopWindowTarget, ShapeVisual},
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, VK_ESCAPE},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, IDC_ARROW, LoadCursorW, RegisterClassW, SW_HIDE,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow,
                WM_APP, WM_CAPTURECHANGED, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{BOOL, Interface, w},
};
use windows_numerics::Vector2;

use crate::{gfx::Gfx, scene::Theme};

thread_local! {
    static CONTROL_CENTER_VIEW: RefCell<Option<ControlCenterView>> = const { RefCell::new(None) };
}

pub struct ControlCenterView {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ShapeVisual,
    layout: ControlCenterLayout,
    scale: f32,
    theme: Theme,
    display_slider: SliderState,
    sound_slider: SliderState,
    brightness: BrightnessControl,
    audio: Option<AudioWatch>,
    active_drag: Option<ControlCenterAction>,
    last_hardware_write: Instant,
}

impl ControlCenterView {
    pub fn toggle(gfx: &Gfx, anchor_x: f32, anchor_y: f32, scale: f32, theme: Theme) -> Result<()> {
        CONTROL_CENTER_VIEW.with(|cell| {
            let mut borrow = cell.borrow_mut();
            if let Some(view) = borrow.as_mut() {
                if view.is_visible() {
                    view.hide();
                    return Ok(());
                }
                view.show(anchor_x, anchor_y, scale, theme)?;
            } else {
                let mut view = Self::new(gfx, anchor_x, anchor_y, scale, theme)?;
                view.show(anchor_x, anchor_y, scale, theme)?;
                *borrow = Some(view);
            }
            Ok(())
        })
    }

    pub fn close_if_open() {
        CONTROL_CENTER_VIEW.with(|cell| {
            if let Some(view) = cell.borrow_mut().as_mut() {
                view.hide();
            }
        });
    }

    fn new(gfx: &Gfx, anchor_x: f32, anchor_y: f32, scale: f32, theme: Theme) -> Result<Self> {
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MuelleControlCenter");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(cc_window_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });

            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                class,
                w!("Control Center"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };

        unsafe {
            let use_backdrop = BOOL::from(true);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&raw const use_backdrop).cast(),
                size_of::<BOOL>() as u32,
            );
        }

        let interop: ICompositorDesktopInterop = gfx.compositor.cast()?;
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false)? };
        let root = gfx.compositor.CreateShapeVisual()?;
        target.SetRoot(&root)?;

        let layout = ControlCenterLayout::compute(anchor_x, anchor_y, scale);
        let brightness = BrightnessControl::new();
        let audio = AudioWatch::new(hwnd.0 as isize, WM_APP + 51);

        Ok(Self {
            hwnd,
            _target: target,
            root,
            layout,
            scale,
            theme,
            display_slider: SliderState::default(),
            sound_slider: SliderState::default(),
            brightness,
            audio,
            active_drag: None,
            last_hardware_write: Instant::now(),
        })
    }

    pub fn is_visible(&self) -> bool {
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn show(&mut self, anchor_x: f32, anchor_y: f32, scale: f32, theme: Theme) -> Result<()> {
        self.scale = scale;
        self.theme = theme;
        self.layout = ControlCenterLayout::compute(anchor_x, anchor_y, scale);

        // Sync initial slider values
        if let Some(b) = self.brightness.get_brightness(self.hwnd) {
            self.display_slider = SliderState::new(b as f32 / 100.0, false, true);
        } else {
            self.display_slider = SliderState::new(1.0, false, false);
        }

        if let Some(vol) = self.audio.as_mut().and_then(|a| a.volume()) {
            self.sound_slider = SliderState::new(vol.percent as f32 / 100.0, vol.muted, true);
        }

        self.render()?;

        let rect = &self.layout.panel_rect;
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                rect.x.round() as i32,
                rect.y.round() as i32,
                rect.width.round() as i32,
                rect.height.round() as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    pub fn hide(&mut self) {
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn render(&mut self) -> Result<()> {
        let compositor = self.root.Compositor()?;
        let shapes = self.root.Shapes()?;
        shapes.Clear()?;

        let px = |logical: f32| (logical * self.scale).round();
        let bg_color = if self.theme.light {
            Color {
                A: 200,
                R: 245,
                G: 245,
                B: 245,
            }
        } else {
            Color {
                A: 200,
                R: 32,
                G: 32,
                B: 32,
            }
        };
        let tile_bg = if self.theme.light {
            Color {
                A: 120,
                R: 255,
                G: 255,
                B: 255,
            }
        } else {
            Color {
                A: 120,
                R: 55,
                G: 55,
                B: 55,
            }
        };

        // 1. Panel Background
        let panel_shape = compositor.CreateSpriteShape()?;
        let panel_geom = compositor.CreateRoundedRectangleGeometry()?;
        panel_geom.SetSize(Vector2::new(
            self.layout.panel_rect.width,
            self.layout.panel_rect.height,
        ))?;
        panel_geom.SetCornerRadius(Vector2::new(px(PANEL_RADIUS), px(PANEL_RADIUS)))?;
        panel_shape.SetGeometry(&panel_geom)?;
        panel_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(bg_color)?)?;
        shapes.Append(&panel_shape)?;

        // 2. Connectivity tile background
        let conn_shape = compositor.CreateSpriteShape()?;
        let conn_geom = compositor.CreateRoundedRectangleGeometry()?;
        let c_rect = &self.layout.connectivity_rect;
        conn_geom.SetSize(Vector2::new(c_rect.width, c_rect.height))?;
        conn_geom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
        conn_shape.SetOffset(Vector2::new(
            c_rect.x - self.layout.panel_rect.x,
            c_rect.y - self.layout.panel_rect.y,
        ))?;
        conn_shape.SetGeometry(&conn_geom)?;
        conn_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(tile_bg)?)?;
        shapes.Append(&conn_shape)?;

        // 3. Display Slider Track & Fill
        let d_rect = &self.layout.display_slider_rect;
        let d_track = compositor.CreateSpriteShape()?;
        let d_geom = compositor.CreateRoundedRectangleGeometry()?;
        d_geom.SetSize(Vector2::new(d_rect.width, d_rect.height))?;
        d_geom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
        d_track.SetOffset(Vector2::new(
            d_rect.x - self.layout.panel_rect.x,
            d_rect.y - self.layout.panel_rect.y,
        ))?;
        d_track.SetGeometry(&d_geom)?;
        d_track.SetFillBrush(&compositor.CreateColorBrushWithColor(tile_bg)?)?;
        shapes.Append(&d_track)?;

        if self.display_slider.supported && self.display_slider.value > 0.0 {
            let fill_shape = compositor.CreateSpriteShape()?;
            let fill_geom = compositor.CreateRoundedRectangleGeometry()?;
            let fill_w = (d_rect.width * self.display_slider.value)
                .max(px(TILE_RADIUS) * 2.0)
                .min(d_rect.width);
            fill_geom.SetSize(Vector2::new(fill_w, d_rect.height))?;
            fill_geom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
            fill_shape.SetOffset(Vector2::new(
                d_rect.x - self.layout.panel_rect.x,
                d_rect.y - self.layout.panel_rect.y,
            ))?;
            fill_shape.SetGeometry(&fill_geom)?;
            let fill_color = Color {
                A: if self.theme.light { 255 } else { 235 },
                R: 255,
                G: 255,
                B: 255,
            };
            fill_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(fill_color)?)?;
            shapes.Append(&fill_shape)?;
        }

        // 4. Sound Slider Track & Fill
        let s_rect = &self.layout.sound_slider_rect;
        let s_track = compositor.CreateSpriteShape()?;
        let s_geom = compositor.CreateRoundedRectangleGeometry()?;
        s_geom.SetSize(Vector2::new(s_rect.width, s_rect.height))?;
        s_geom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
        s_track.SetOffset(Vector2::new(
            s_rect.x - self.layout.panel_rect.x,
            s_rect.y - self.layout.panel_rect.y,
        ))?;
        s_track.SetGeometry(&s_geom)?;
        s_track.SetFillBrush(&compositor.CreateColorBrushWithColor(tile_bg)?)?;
        shapes.Append(&s_track)?;

        if self.sound_slider.supported && self.sound_slider.value > 0.0 && !self.sound_slider.muted
        {
            let s_fill = compositor.CreateSpriteShape()?;
            let s_fgeom = compositor.CreateRoundedRectangleGeometry()?;
            let s_w = (s_rect.width * self.sound_slider.value)
                .max(px(TILE_RADIUS) * 2.0)
                .min(s_rect.width);
            s_fgeom.SetSize(Vector2::new(s_w, s_rect.height))?;
            s_fgeom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
            s_fill.SetOffset(Vector2::new(
                s_rect.x - self.layout.panel_rect.x,
                s_rect.y - self.layout.panel_rect.y,
            ))?;
            s_fill.SetGeometry(&s_fgeom)?;
            let fill_color = Color {
                A: if self.theme.light { 255 } else { 235 },
                R: 255,
                G: 255,
                B: 255,
            };
            s_fill.SetFillBrush(&compositor.CreateColorBrushWithColor(fill_color)?)?;
            shapes.Append(&s_fill)?;
        }

        // 5. Shortcuts row
        let short_rect = &self.layout.shortcuts_rect;
        let short_shape = compositor.CreateSpriteShape()?;
        let short_geom = compositor.CreateRoundedRectangleGeometry()?;
        short_geom.SetSize(Vector2::new(short_rect.width, short_rect.height))?;
        short_geom.SetCornerRadius(Vector2::new(px(TILE_RADIUS), px(TILE_RADIUS)))?;
        short_shape.SetOffset(Vector2::new(
            short_rect.x - self.layout.panel_rect.x,
            short_rect.y - self.layout.panel_rect.y,
        ))?;
        short_shape.SetGeometry(&short_geom)?;
        short_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(tile_bg)?)?;
        shapes.Append(&short_shape)?;

        self.root.SetSize(Vector2::new(
            self.layout.panel_rect.width,
            self.layout.panel_rect.height,
        ))?;
        Ok(())
    }

    fn on_mouse_down(&mut self, local_x: f32, local_y: f32) -> Result<()> {
        let abs_x = self.layout.panel_rect.x + local_x;
        let abs_y = self.layout.panel_rect.y + local_y;

        if self.layout.display_slider_rect.contains(abs_x, abs_y) && self.display_slider.supported {
            self.active_drag = Some(ControlCenterAction::DisplaySlider);
            self.display_slider.set_from_coordinate(
                abs_x - self.layout.display_slider_rect.x,
                self.layout.display_slider_rect.width,
            );
            self.apply_brightness();
            unsafe {
                SetCapture(self.hwnd);
            }
            self.render()?;
        } else if self.layout.sound_slider_rect.contains(abs_x, abs_y)
            && self.sound_slider.supported
        {
            self.active_drag = Some(ControlCenterAction::SoundSlider);
            self.sound_slider.set_from_coordinate(
                abs_x - self.layout.sound_slider_rect.x,
                self.layout.sound_slider_rect.width,
            );
            self.apply_volume();
            unsafe {
                SetCapture(self.hwnd);
            }
            self.render()?;
        } else if self.layout.shortcuts_rect.contains(abs_x, abs_y) {
            let rel_x = abs_x - self.layout.shortcuts_rect.x;
            let slot_w = self.layout.shortcuts_rect.width / 5.0;
            let idx = (rel_x / slot_w) as usize;
            match idx {
                0 => {
                    let _ = Personalization::toggle_theme();
                }
                1 => Shortcuts::open_screenshot(),
                2 => Shortcuts::open_calculator(),
                3 => Shortcuts::open_night_light(),
                4 => Shortcuts::open_battery_saver(),
                _ => {}
            }
        }
        Ok(())
    }

    fn on_mouse_move(&mut self, local_x: f32, _local_y: f32) -> Result<()> {
        let abs_x = self.layout.panel_rect.x + local_x;
        match self.active_drag {
            Some(ControlCenterAction::DisplaySlider) => {
                self.display_slider.set_from_coordinate(
                    abs_x - self.layout.display_slider_rect.x,
                    self.layout.display_slider_rect.width,
                );
                if self.last_hardware_write.elapsed().as_millis() > 40 {
                    self.apply_brightness();
                    self.last_hardware_write = Instant::now();
                }
                self.render()?;
            }
            Some(ControlCenterAction::SoundSlider) => {
                self.sound_slider.set_from_coordinate(
                    abs_x - self.layout.sound_slider_rect.x,
                    self.layout.sound_slider_rect.width,
                );
                if self.last_hardware_write.elapsed().as_millis() > 40 {
                    self.apply_volume();
                    self.last_hardware_write = Instant::now();
                }
                self.render()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn on_mouse_up(&mut self) -> Result<()> {
        if self.active_drag.is_some() {
            unsafe {
                if GetCapture() == self.hwnd {
                    let _ = ReleaseCapture();
                }
            }
            match self.active_drag.take() {
                Some(ControlCenterAction::DisplaySlider) => self.apply_brightness(),
                Some(ControlCenterAction::SoundSlider) => self.apply_volume(),
                _ => {}
            }
            self.render()?;
        }
        Ok(())
    }

    fn apply_brightness(&self) {
        let percent = (self.display_slider.value * 100.0).round() as u32;
        let _ = self.brightness.set_brightness(self.hwnd, percent);
    }

    fn apply_volume(&mut self) {
        if let Some(audio) = self.audio.as_mut() {
            let percent = (self.sound_slider.value * 100.0).round() as u8;
            let _ = audio.set_percent(percent);
        }
    }
}

unsafe extern "system" fn cc_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            CONTROL_CENTER_VIEW.with(|cell| {
                if let Some(view) = cell.borrow_mut().as_mut() {
                    let _ = view.on_mouse_down(x, y);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            CONTROL_CENTER_VIEW.with(|cell| {
                if let Some(view) = cell.borrow_mut().as_mut() {
                    let _ = view.on_mouse_move(x, y);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            CONTROL_CENTER_VIEW.with(|cell| {
                if let Some(view) = cell.borrow_mut().as_mut() {
                    let _ = view.on_mouse_up();
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            ControlCenterView::close_if_open();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            CONTROL_CENTER_VIEW.with(|cell| {
                if let Some(view) = cell.borrow_mut().as_mut() {
                    view.active_drag = None;
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
