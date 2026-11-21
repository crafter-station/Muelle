//! Hardware and system integration for the macOS-style Control Center panel.
//!
//! Provides brightness, audio volume, power, personalization (dark mode),
//! and quick shortcuts.

use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, WPARAM},
        Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryW},
            Registry::{
                HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, RRF_RT_REG_DWORD, RegCloseKey,
                RegGetValueW, RegOpenKeyExW, RegSetValueExW,
            },
        },
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{HWND_BROADCAST, SW_SHOW, SendNotifyMessageW, WM_SETTINGCHANGE},
        },
    },
    core::{BOOL, PCWSTR, w},
};

/// High-level brightness provider for physical and built-in displays.
type ProcFn = unsafe extern "system" fn() -> isize;
type GetNumMonitorsFn = unsafe extern "system" fn(HMONITOR, *mut u32) -> BOOL;
type GetPhysicalMonitorsFn = unsafe extern "system" fn(HMONITOR, u32, *mut PhysicalMonitor) -> BOOL;
type GetMonitorBrightnessFn =
    unsafe extern "system" fn(isize, *mut u32, *mut u32, *mut u32) -> BOOL;
type SetMonitorBrightnessFn = unsafe extern "system" fn(isize, u32) -> BOOL;
type DestroyPhysicalMonitorsFn = unsafe extern "system" fn(u32, *mut PhysicalMonitor) -> BOOL;

pub struct BrightnessControl {
    _dxva2: Option<HMODULE>,
    get_num: Option<GetNumMonitorsFn>,
    get_phys: Option<GetPhysicalMonitorsFn>,
    get_bright: Option<GetMonitorBrightnessFn>,
    set_bright: Option<SetMonitorBrightnessFn>,
    destroy_phys: Option<DestroyPhysicalMonitorsFn>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PhysicalMonitor {
    handle: isize,
    description: [u16; 128],
}

impl Default for BrightnessControl {
    fn default() -> Self {
        Self::new()
    }
}

impl BrightnessControl {
    #[must_use]
    pub fn new() -> Self {
        // SAFETY: Loading system dxva2.dll dynamically so systems without it continue running.
        unsafe {
            let dxva2 = LoadLibraryW(w!("dxva2.dll")).ok();
            let mut ctrl = Self {
                _dxva2: dxva2,
                get_num: None,
                get_phys: None,
                get_bright: None,
                set_bright: None,
                destroy_phys: None,
            };
            if let Some(dll) = dxva2 {
                let load =
                    |name: PCWSTR| GetProcAddress(dll, windows::core::PCSTR(name.as_ptr().cast()));
                if let Some(f) = load(w!("GetNumberOfPhysicalMonitorsFromHMONITOR")) {
                    ctrl.get_num = Some(std::mem::transmute::<ProcFn, GetNumMonitorsFn>(f));
                }
                if let Some(f) = load(w!("GetPhysicalMonitorsFromHMONITOR")) {
                    ctrl.get_phys = Some(std::mem::transmute::<ProcFn, GetPhysicalMonitorsFn>(f));
                }
                if let Some(f) = load(w!("GetMonitorBrightness")) {
                    ctrl.get_bright =
                        Some(std::mem::transmute::<ProcFn, GetMonitorBrightnessFn>(f));
                }
                if let Some(f) = load(w!("SetMonitorBrightness")) {
                    ctrl.set_bright =
                        Some(std::mem::transmute::<ProcFn, SetMonitorBrightnessFn>(f));
                }
                if let Some(f) = load(w!("DestroyPhysicalMonitors")) {
                    ctrl.destroy_phys =
                        Some(std::mem::transmute::<ProcFn, DestroyPhysicalMonitorsFn>(f));
                }
            }
            ctrl
        }
    }

    /// Reads brightness level (0..=100) of the primary monitor.
    /// Returns `None` if brightness control is unsupported on this hardware.
    #[must_use]
    pub fn get_brightness(&self, hwnd: HWND) -> Option<u32> {
        let (get_num, get_phys, get_bright, destroy_phys) = (
            self.get_num?,
            self.get_phys?,
            self.get_bright?,
            self.destroy_phys?,
        );

        // SAFETY: Read-only query on active primary monitor.
        unsafe {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
            let mut count = 0u32;
            if !get_num(hmon, &mut count).as_bool() || count == 0 {
                return None;
            }

            let mut monitors = vec![
                PhysicalMonitor {
                    handle: 0,
                    description: [0; 128],
                };
                count as usize
            ];

            if !get_phys(hmon, count, monitors.as_mut_ptr()).as_bool() {
                return None;
            }

            let mut min = 0u32;
            let mut cur = 0u32;
            let mut max = 0u32;
            let res = get_bright(monitors[0].handle, &mut min, &mut cur, &mut max);
            let _ = destroy_phys(count, monitors.as_mut_ptr());

            if res.as_bool() && max > min {
                Some(((cur - min) * 100 / (max - min)).clamp(0, 100))
            } else {
                None
            }
        }
    }

    /// Sets brightness level (0..=100) on the primary monitor.
    pub fn set_brightness(&self, hwnd: HWND, percent: u32) -> bool {
        let (Some(get_num), Some(get_phys), Some(get_bright), Some(set_bright), Some(destroy_phys)) = (
            self.get_num,
            self.get_phys,
            self.get_bright,
            self.set_bright,
            self.destroy_phys,
        ) else {
            return false;
        };

        // SAFETY: Hardware write clamped safely to monitor's reported range.
        unsafe {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
            let mut count = 0u32;
            if !get_num(hmon, &mut count).as_bool() || count == 0 {
                return false;
            }

            let mut monitors = vec![
                PhysicalMonitor {
                    handle: 0,
                    description: [0; 128],
                };
                count as usize
            ];

            if !get_phys(hmon, count, monitors.as_mut_ptr()).as_bool() {
                return false;
            }

            let mut min = 0u32;
            let mut cur = 0u32;
            let mut max = 0u32;
            let ok = if get_bright(monitors[0].handle, &mut min, &mut cur, &mut max).as_bool()
                && max > min
            {
                let target = min + (percent.clamp(0, 100) * (max - min) / 100);
                set_bright(monitors[0].handle, target).as_bool()
            } else {
                false
            };
            let _ = destroy_phys(count, monitors.as_mut_ptr());
            ok
        }
    }
}

/// Reads and toggles Windows Dark/Light mode theme.
pub struct Personalization;

impl Personalization {
    const SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

    /// Checks if Windows is currently using light theme for apps.
    #[must_use]
    pub fn is_light_theme() -> bool {
        // SAFETY: Read-only registry query.
        unsafe {
            let mut val = 0u32;
            let mut size = size_of::<u32>() as u32;
            let res = RegGetValueW(
                HKEY_CURRENT_USER,
                Self::SUBKEY,
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut val).cast()),
                Some(&mut size),
            );
            res.is_ok() && val != 0
        }
    }

    /// Toggles Windows Dark/Light mode theme.
    pub fn toggle_theme() -> bool {
        let target = u32::from(!Self::is_light_theme());
        let bytes = target.to_ne_bytes();
        // SAFETY: Documented personalization registry write and notification broadcast.
        unsafe {
            let mut key = windows::Win32::System::Registry::HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                Self::SUBKEY,
                None,
                KEY_SET_VALUE,
                &mut key,
            )
            .is_ok()
            {
                let _ = RegSetValueExW(key, w!("AppsUseLightTheme"), None, REG_DWORD, Some(&bytes));
                let _ = RegSetValueExW(
                    key,
                    w!("SystemUsesLightTheme"),
                    None,
                    REG_DWORD,
                    Some(&bytes),
                );
                let _ = RegCloseKey(key);

                // Notify open top-level windows of color change
                let _ = SendNotifyMessageW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    WPARAM(0),
                    LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
                );
                true
            } else {
                false
            }
        }
    }
}

/// Launches standard Windows tools and settings pages.
pub struct Shortcuts;

impl Shortcuts {
    pub fn open_screenshot() {
        Self::launch(w!("ms-screenclip:"));
    }

    pub fn open_calculator() {
        Self::launch(w!("calc.exe"));
    }

    pub fn open_clock() {
        Self::launch(w!("ms-clock:"));
    }

    pub fn open_night_light() {
        Self::launch(w!("ms-settings:nightlight"));
    }

    pub fn open_battery_saver() {
        Self::launch(w!("ms-settings:batterysaver"));
    }

    pub fn open_sound_settings() {
        Self::launch(w!("ms-settings:sound"));
    }

    pub fn open_network_settings() {
        Self::launch(w!("ms-settings:network"));
    }

    pub fn open_bluetooth_settings() {
        Self::launch(w!("ms-settings:bluetooth"));
    }

    fn launch(target: PCWSTR) {
        // SAFETY: Documented ShellExecuteW to launch system tools.
        unsafe {
            let _ = ShellExecuteW(None, w!("open"), target, None, None, SW_SHOW);
        }
    }
}
