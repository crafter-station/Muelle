//! What the control center reads and changes: the Wi-Fi and Bluetooth radios,
//! the screen's brightness, Do Not Disturb and Windows' light or dark mode.
//!
//! Every call here can take a while (radios answer asynchronously, a monitor
//! answers over its cable), so none belongs on a UI thread.

use windows::{
    Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState},
    Networking::Connectivity::NetworkInformation,
    Win32::{
        Devices::Display::{
            DestroyPhysicalMonitor, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
        },
        Foundation::{E_ACCESSDENIED, E_FAIL, HANDLE, LPARAM, POINT, WPARAM},
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree},
        System::Registry::{
            HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
        },
        UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        },
    },
    core::{
        Error, GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, PWSTR, Result, interface, w,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioSwitch {
    WiFi,
    Bluetooth,
}

fn radios(switch: RadioSwitch) -> Result<Vec<Radio>> {
    let kind = match switch {
        RadioSwitch::WiFi => RadioKind::WiFi,
        RadioSwitch::Bluetooth => RadioKind::Bluetooth,
    };
    let all = Radio::GetRadiosAsync()?.join()?;
    Ok(all
        .into_iter()
        .filter(|radio| radio.Kind().is_ok_and(|found| found == kind))
        .collect())
}

/// Whether any radio of this kind is on; `None` if this PC has none.
#[must_use]
pub fn radio_on(switch: RadioSwitch) -> Option<bool> {
    let found = radios(switch).ok()?;
    (!found.is_empty()).then(|| {
        found
            .iter()
            .any(|radio| radio.State().is_ok_and(|state| state == RadioState::On))
    })
}

/// Turns every radio of this kind on or off, as quick settings does.
///
/// # Errors
/// If Windows does not let this program control radios, or a radio refuses.
pub fn set_radio(switch: RadioSwitch, on: bool) -> Result<()> {
    if Radio::RequestAccessAsync()?.join()? != RadioAccessStatus::Allowed {
        return Err(E_ACCESSDENIED.into());
    }
    let state = if on { RadioState::On } else { RadioState::Off };
    for radio in radios(switch)? {
        if radio.SetStateAsync(state)?.join()? != RadioAccessStatus::Allowed {
            return Err(E_ACCESSDENIED.into());
        }
    }
    Ok(())
}

/// The name of the network Windows uses to reach the internet: the Wi-Fi
/// network's, or the wired connection's.
#[must_use]
pub fn network_name() -> Option<String> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    let name = profile.ProfileName().ok()?.to_string();
    (!name.trim().is_empty()).then_some(name)
}

/// The primary monitor's brightness, over DDC/CI. External monitors answer
/// it when it is enabled in their own menu; built-in laptop panels do not.
pub struct ScreenBrightness {
    monitor: HANDLE,
    low: u32,
    high: u32,
}

impl ScreenBrightness {
    /// The primary monitor, if its brightness can be set.
    #[must_use]
    pub fn primary() -> Option<Self> {
        // SAFETY: The origin is on the primary monitor. Every handle obtained
        // is either kept in `Self`, released by `Drop`, or released here.
        unsafe {
            let screen = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut count = 0;
            GetNumberOfPhysicalMonitorsFromHMONITOR(screen, &mut count).ok()?;
            let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
            GetPhysicalMonitorsFromHMONITOR(screen, &mut monitors).ok()?;
            let mut chosen = None;
            for physical in monitors {
                let monitor = physical.hPhysicalMonitor;
                let (mut low, mut current, mut high) = (0, 0, 0);
                if chosen.is_none()
                    && GetMonitorBrightness(monitor, &mut low, &mut current, &mut high) != 0
                    && high > low
                {
                    chosen = Some(Self { monitor, low, high });
                } else {
                    let _ = DestroyPhysicalMonitor(monitor);
                }
            }
            chosen
        }
    }

    /// From 0 to 100 within the monitor's own range.
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        let (mut low, mut current, mut high) = (0, 0, 0);
        // SAFETY: Our own live monitor handle and valid outputs.
        let read = unsafe { GetMonitorBrightness(self.monitor, &mut low, &mut current, &mut high) };
        (read != 0).then(|| to_percent(current, self.low, self.high))
    }

    /// # Errors
    /// If the monitor does not accept the value.
    pub fn set_percent(&self, percent: u8) -> Result<()> {
        let value = from_percent(percent, self.low, self.high);
        // SAFETY: Our own live monitor handle.
        if unsafe { SetMonitorBrightness(self.monitor, value) } == 0 {
            return Err(Error::from_hresult(E_FAIL));
        }
        Ok(())
    }
}

impl Drop for ScreenBrightness {
    fn drop(&mut self) {
        // SAFETY: Releases the handle taken in `primary`, once.
        unsafe {
            let _ = DestroyPhysicalMonitor(self.monitor);
        }
    }
}

fn to_percent(value: u32, low: u32, high: u32) -> u8 {
    let span = high.saturating_sub(low).max(1);
    ((value.clamp(low, high.max(low)) - low) * 100 + span / 2).div_euclid(span) as u8
}

fn from_percent(percent: u8, low: u32, high: u32) -> u32 {
    let span = high.saturating_sub(low);
    low + (span * u32::from(percent.min(100)) + 50) / 100
}

/// Windows has no documented switch for Do Not Disturb. This is the service
/// interface its notification center uses; it names the mode as a "profile".
#[interface("6bff4732-81ec-4ffb-ae67-b6c1bc29631f")]
unsafe trait IQuietHoursSettings: IUnknown {
    fn get_user_selected_profile(&self, profile: *mut PWSTR) -> HRESULT;
    fn set_user_selected_profile(&self, profile: PCWSTR) -> HRESULT;
}

const QUIET_HOURS_SETTINGS: GUID = GUID::from_u128(0xf53321fa_34f8_4b7f_b9a3_361877cb94cf);
/// Everything comes through.
const UNRESTRICTED: &str = "Microsoft.QuietHoursProfile.Unrestricted";
/// Windows 11's "Do not disturb" (Windows 10's "Priority only").
const PRIORITY_ONLY: PCWSTR = w!("Microsoft.QuietHoursProfile.PriorityOnly");

fn quiet_hours() -> Result<IQuietHoursSettings> {
    // SAFETY: Asks the notification service for its settings object.
    unsafe { CoCreateInstance(&QUIET_HOURS_SETTINGS, None, CLSCTX_LOCAL_SERVER) }
}

/// Whether Do Not Disturb is on; `None` if this Windows does not answer.
/// COM must be initialized on this thread.
#[must_use]
pub fn do_not_disturb() -> Option<bool> {
    let settings = quiet_hours().ok()?;
    let mut profile = PWSTR::null();
    // SAFETY: On success the service allocates the string, freed here.
    unsafe {
        settings.get_user_selected_profile(&mut profile).ok().ok()?;
        let name = profile.to_string().ok();
        CoTaskMemFree(Some(profile.0.cast()));
        name.map(|name| name != UNRESTRICTED)
    }
}

/// Turns Do Not Disturb on or off, as the notification center does.
///
/// # Errors
/// If this Windows does not offer the switch.
pub fn set_do_not_disturb(on: bool) -> Result<()> {
    let off = HSTRING::from(UNRESTRICTED);
    let profile = if on {
        PRIORITY_ONLY
    } else {
        PCWSTR(off.as_ptr())
    };
    // SAFETY: Both names outlive the call.
    unsafe { quiet_hours()?.set_user_selected_profile(profile).ok() }
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

/// Whether Windows apps are set to dark mode.
#[must_use]
pub fn dark_mode() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` describe a valid 4-byte buffer.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    read.is_ok() && value == 0
}

/// Switches both apps and the Windows shell, and tells running programs, as
/// Settings → Personalization → Colors does.
///
/// # Errors
/// If the preference cannot be saved.
pub fn set_dark_mode(dark: bool) -> Result<()> {
    let light = u32::from(!dark);
    for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        // SAFETY: Writes one DWORD under the user's own key.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                name,
                REG_DWORD.0,
                Some((&raw const light).cast()),
                size_of::<u32>() as u32,
            )
        }
        .ok()?;
    }
    let area = w!("ImmersiveColorSet");
    // SAFETY: The string is static; hung windows are skipped.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(area.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            100,
            None,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_percentages_follow_the_monitors_range() {
        assert_eq!(to_percent(0, 0, 100), 0);
        assert_eq!(to_percent(75, 50, 150), 25);
        assert_eq!(to_percent(500, 50, 150), 100);
        assert_eq!(from_percent(25, 50, 150), 75);
        assert_eq!(from_percent(255, 0, 80), 80);
        for percent in [0, 1, 33, 50, 99, 100] {
            assert_eq!(to_percent(from_percent(percent, 0, 100), 0, 100), percent);
        }
        // A monitor reporting no range must not divide by zero.
        assert_eq!(to_percent(7, 7, 7), 0);
    }

    #[test]
    #[ignore = "Read-only queries of this PC's radios, monitor and theme"]
    fn native_controls_report_this_pc() {
        println!("Wi-Fi: {:?}", radio_on(RadioSwitch::WiFi));
        println!("Bluetooth: {:?}", radio_on(RadioSwitch::Bluetooth));
        println!("network: {:?}", network_name().map(|name| name.len()));
        println!(
            "brightness: {:?}",
            ScreenBrightness::primary().and_then(|screen| screen.percent())
        );
        println!("dark mode: {}", dark_mode());
        // SAFETY: This test thread's own apartment.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }
        println!("do not disturb: {:?}", do_not_disturb());
    }

    #[test]
    #[ignore = "Switches Do Not Disturb and switches it back"]
    fn native_do_not_disturb_switches_and_returns() -> Result<()> {
        // SAFETY: This test thread's own apartment.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }
        let Some(before) = do_not_disturb() else {
            println!("not offered here");
            return Ok(());
        };
        set_do_not_disturb(!before)?;
        let switched = do_not_disturb();
        set_do_not_disturb(before)?;
        assert_eq!(switched, Some(!before));
        assert_eq!(do_not_disturb(), Some(before));
        Ok(())
    }
}
