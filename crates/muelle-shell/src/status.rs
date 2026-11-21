//! System status for the menu bar: the network, the output volume, and the
//! buttons the user has turned on for the Windows taskbar.
//!
//! Nothing here polls. Windows reports network and volume changes through
//! callbacks on its own threads; they only post `message` to the bar's window,
//! which then reads the new state on its UI thread.

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Networking::Connectivity::{
        NetworkConnectivityLevel, NetworkInformation, NetworkStatusChangedEventHandler,
    },
    Win32::{
        Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM},
        Media::Audio::{
            AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
            MMDeviceEnumerator, eCommunications, eConsole, eMultimedia, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
    core::{
        GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, Result, implement, interface, w,
    },
};

fn notify(window: isize, message: u32) {
    // SAFETY: Only posts a message; a window that no longer exists ignores it.
    unsafe {
        let _ = PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0));
    }
}

/* ---------------------------------------------------------------- Network */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Offline,
    Ethernet,
    /// Signal strength from 1 to 4 bars, and whether connected to internet.
    Wifi {
        bars: u8,
        has_internet: bool,
    },
}

/// The connection Windows uses to reach the internet.
#[must_use]
pub fn network() -> Network {
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
        return Network::Offline;
    };
    let level = profile
        .GetNetworkConnectivityLevel()
        .unwrap_or(NetworkConnectivityLevel::None);
    if level == NetworkConnectivityLevel::None {
        return Network::Offline;
    }
    let has_internet = level == NetworkConnectivityLevel::InternetAccess;
    if profile.IsWlanConnectionProfile().unwrap_or(false) {
        // Windows reports 0 to 5 bars; the glyphs have 4 steps.
        let reported = profile
            .GetSignalBars()
            .and_then(|bars| bars.Value())
            .unwrap_or(5);
        let bars = (u32::from(reported.min(5)) * 4).div_ceil(5).max(1) as u8;
        Network::Wifi { bars, has_internet }
    } else {
        Network::Ethernet
    }
}

/// A connected wired (Ethernet) network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WiredConnection {
    /// The name Windows gives the network, e.g. "Network 3".
    pub name: String,
    /// Whether it reaches the internet, not only the local network.
    pub has_internet: bool,
}

/// Connected wired networks; Wi-Fi and mobile connections are left out.
#[must_use]
pub fn wired_connections() -> Vec<WiredConnection> {
    let Ok(profiles) = NetworkInformation::GetConnectionProfiles() else {
        return Vec::new();
    };
    let mut wired = Vec::new();
    for profile in profiles {
        let wireless = profile.IsWlanConnectionProfile().unwrap_or(true)
            || profile.IsWwanConnectionProfile().unwrap_or(true);
        let level = profile
            .GetNetworkConnectivityLevel()
            .unwrap_or(NetworkConnectivityLevel::None);
        if wireless || level == NetworkConnectivityLevel::None {
            continue;
        }
        let name = profile
            .ProfileName()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        wired.push(WiredConnection {
            name,
            has_internet: level == NetworkConnectivityLevel::InternetAccess,
        });
    }
    wired
}

/// Posts `message` to `window` whenever the network changes.
pub struct NetworkWatch {
    token: i64,
}

impl NetworkWatch {
    #[must_use]
    pub fn new(window: isize, message: u32) -> Option<Self> {
        let handler = NetworkStatusChangedEventHandler::new(move |_| {
            notify(window, message);
            Ok(())
        });
        NetworkInformation::NetworkStatusChanged(&handler)
            .ok()
            .map(|token| Self { token })
    }
}

impl Drop for NetworkWatch {
    fn drop(&mut self) {
        let _ = NetworkInformation::RemoveNetworkStatusChanged(self.token);
    }
}

/* ----------------------------------------------------------------- Volume */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    pub muted: bool,
    /// 0 (silent) to 3 (loud): the steps the speaker glyph has.
    pub level: u8,
    /// Precise endpoint reading, retained for the music popup slider.
    pub percent: u8,
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeChanged {
    window: isize,
    message: u32,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeChanged_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        notify(self.window, self.message);
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceChanged {
    window: isize,
    message: u32,
    default_changed: Arc<AtomicBool>,
}

impl IMMNotificationClient_Impl for DeviceChanged_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            self.default_changed.store(true, Ordering::Release);
            notify(self.window, self.message);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// The default speakers' volume, followed across device changes.
pub struct AudioWatch {
    default_changed: Arc<AtomicBool>,
    enumerator: IMMDeviceEnumerator,
    devices: IMMNotificationClient,
    /// Current default device, its volume control and our callback on it.
    endpoint: Option<(String, IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
    window: isize,
    message: u32,
}

impl AudioWatch {
    /// Must be created on a thread with COM initialized (the UI thread).
    #[must_use]
    pub fn new(window: isize, message: u32) -> Option<Self> {
        // SAFETY: COM is initialized on this thread; the enumerator and the
        // registered client are released in `Drop`.
        let default_changed = Arc::new(AtomicBool::new(false));
        let (enumerator, devices) = unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let devices: IMMNotificationClient = DeviceChanged {
                window,
                message,
                default_changed: default_changed.clone(),
            }
            .into();
            enumerator
                .RegisterEndpointNotificationCallback(&devices)
                .ok()?;
            (enumerator, devices)
        };
        Some(Self {
            default_changed,
            enumerator,
            devices,
            endpoint: None,
            window,
            message,
        })
    }

    pub fn take_default_changed(&self) -> bool {
        self.default_changed.swap(false, Ordering::AcqRel)
    }

    /// Current volume of the default speakers, or `None` without any.
    pub fn volume(&mut self) -> Option<Volume> {
        self.follow_default();
        let (_, endpoint, _) = self.endpoint.as_ref()?;
        // SAFETY: Plain queries on a live COM object.
        let (level, muted) = unsafe {
            (
                endpoint.GetMasterVolumeLevelScalar().ok()?,
                endpoint.GetMute().ok()?.as_bool(),
            )
        };
        let percent = (level.clamp(0.0, 1.0) * 100.0).round() as u8;
        let level = match level {
            l if l < 0.01 => 0,
            l if l < 0.34 => 1,
            l if l < 0.67 => 2,
            _ => 3,
        };
        Some(Volume {
            muted,
            level,
            percent,
        })
    }

    /// Uses the watched default output; the existing callback updates all UI.
    pub fn set_percent(&mut self, percent: u8) -> Result<()> {
        self.follow_default();
        let (_, endpoint, _) = self.endpoint.as_ref().ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
        })?;
        // SAFETY: The endpoint belongs to this COM-initialized thread. The
        // event context is optional. No waits, enumeration or new watcher.
        unsafe {
            endpoint.SetMasterVolumeLevelScalar(
                f32::from(percent.min(100)) / 100.0,
                std::ptr::null(),
            )?;
            if percent > 0 && endpoint.GetMute()?.as_bool() {
                endpoint.SetMute(false, std::ptr::null())?;
            }
        }
        Ok(())
    }

    pub fn toggle_mute(&mut self) -> Result<()> {
        self.follow_default();
        let (_, endpoint, _) = self.endpoint.as_ref().ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
        })?;
        // SAFETY: Read and update our current, live endpoint on its apartment.
        unsafe { endpoint.SetMute(!endpoint.GetMute()?.as_bool(), std::ptr::null()) }
    }

    /// Moves the volume callback to the default device if it changed.
    fn follow_default(&mut self) {
        // SAFETY: COM calls on live objects; the id string is freed with
        // CoTaskMemFree as the API requires.
        unsafe {
            let Ok(device) = self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else {
                self.unbind();
                return;
            };
            let Ok(raw) = device.GetId() else {
                return;
            };
            let id = raw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw.0.cast()));
            if self
                .endpoint
                .as_ref()
                .is_some_and(|(current, ..)| *current == id)
            {
                return;
            }
            self.unbind();
            let Ok(endpoint) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else {
                return;
            };
            let callback: IAudioEndpointVolumeCallback = VolumeChanged {
                window: self.window,
                message: self.message,
            }
            .into();
            if endpoint.RegisterControlChangeNotify(&callback).is_ok() {
                self.endpoint = Some((id, endpoint, callback));
            }
        }
    }

    fn unbind(&mut self) {
        if let Some((_, endpoint, callback)) = self.endpoint.take() {
            // SAFETY: Unregisters the callback registered on this endpoint.
            unsafe {
                let _ = endpoint.UnregisterControlChangeNotify(&callback);
            }
        }
    }
}

impl Drop for AudioWatch {
    fn drop(&mut self) {
        self.unbind();
        // SAFETY: Unregisters the client registered in `new`.
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.devices);
        }
    }
}

/* ---------------------------------------------------------- Output devices */

/// An active audio output (speakers, headphones...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOutput {
    /// Endpoint id, the value [`set_default_output`] takes.
    pub id: String,
    pub name: String,
    /// It is the default output for the console role.
    pub default: bool,
}

/// Reads a COM-allocated string and frees it.
///
/// # Safety
/// `raw` must be null or a string allocated with CoTaskMemAlloc that is not
/// used afterwards.
unsafe fn take_com_string(raw: windows::core::PWSTR) -> String {
    // SAFETY: Guaranteed by the caller.
    unsafe {
        let text = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0.cast()));
        text
    }
}

/// Active output devices. Needs COM initialized on this thread; empty on any
/// failure.
#[must_use]
pub fn output_devices() -> Vec<AudioOutput> {
    use windows::Win32::System::Com::{
        STGM_READ,
        StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
    };
    // PKEY_Device_FriendlyName, defined here to avoid a whole feature for it.
    const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    // SAFETY: Read-only Core Audio queries on live COM objects. Every id and
    // name string is freed once with CoTaskMemFree, and the property variant
    // is cleared after use.
    unsafe {
        let Ok(enumerator): Result<IMMDeviceEnumerator> =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return Vec::new();
        };
        let default_id = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .and_then(|device| device.GetId())
            .map(|raw| take_com_string(raw))
            .ok();
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return Vec::new();
        };
        let count = devices.GetCount().unwrap_or(0);
        let mut outputs = Vec::new();
        for index in 0..count {
            let Ok(device) = devices.Item(index) else {
                continue;
            };
            let Ok(raw) = device.GetId() else {
                continue;
            };
            let id = take_com_string(raw);
            let name = device
                .OpenPropertyStore(STGM_READ)
                .and_then(|store| store.GetValue(&FRIENDLY_NAME))
                .ok()
                .and_then(|mut value| {
                    let text = PropVariantToStringAlloc(&value)
                        .ok()
                        .map(|raw| take_com_string(raw));
                    let _ = PropVariantClear(&mut value);
                    text
                })
                .unwrap_or_default();
            let default = default_id.as_deref() == Some(id.as_str());
            outputs.push(AudioOutput { id, name, default });
        }
        outputs
    }
}

/// [`output_devices`] for a worker thread: initializes COM for the call.
#[must_use]
pub fn output_devices_on_worker() -> Vec<AudioOutput> {
    // SAFETY: Balanced below when it succeeded; a thread that already runs
    // another apartment is used as it is.
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let outputs = output_devices();
    if initialized {
        // SAFETY: Balances the successful `CoInitializeEx` above.
        unsafe { CoUninitialize() };
    }
    outputs
}

/// Undocumented Windows interface behind the Sound settings' "set default".
/// Slots before `SetDefaultEndpoint` only keep the vtable offsets right.
#[allow(non_snake_case)]
mod policy_config {
    use super::{ERole, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, PROPERTYKEY, c_void, interface};

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
        fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
        fn GetProcessingPeriod(
            &self,
            id: PCWSTR,
            default: i32,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> HRESULT;
        fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn GetPropertyValue(
            &self,
            id: PCWSTR,
            fx_store: i32,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> HRESULT;
        fn SetPropertyValue(
            &self,
            id: PCWSTR,
            fx_store: i32,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> HRESULT;
        fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }

    /// Calls `SetDefaultEndpoint`, whose generated method is private to this
    /// module.
    ///
    /// # Safety
    /// COM must be initialized and `id` must be a live NUL-terminated string.
    pub unsafe fn set_default(policy: &IPolicyConfig, id: PCWSTR, role: ERole) -> HRESULT {
        // SAFETY: Upheld by the caller.
        unsafe { policy.SetDefaultEndpoint(id, role) }
    }
}
use policy_config::IPolicyConfig;

/// Makes the output device `id` (from [`AudioOutput::id`]) the default for
/// every role. The volume watcher follows through its default-change callback.
pub fn set_default_output(id: &str) -> Result<()> {
    const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
    let id = HSTRING::from(id);
    // SAFETY: COM is initialized on this thread. `id` outlives the calls and is
    // a NUL-terminated wide string; the interface layout matches the vtable.
    unsafe {
        let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
        for role in [eConsole, eMultimedia, eCommunications] {
            policy_config::set_default(&policy, PCWSTR(id.as_ptr()), role).ok()?;
        }
    }
    Ok(())
}

/* ------------------------------------------------------ Taskbar buttons */

/// Cached physical container of the default output. Query from a background
/// reader with COM initialized; a source failure is not a device disconnect.
#[must_use]
pub fn default_audio_container() -> Option<String> {
    use windows::Win32::{
        Foundation::PROPERTYKEY,
        System::Com::{
            STGM_READ,
            StructuredStorage::{PropVariantClear, PropVariantToGUID},
        },
    };
    const CONTAINER: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c),
        pid: 2,
    };
    // SAFETY: Read-only Core Audio property store. Its allocated variant is
    // cleared on both success and conversion failure before returning.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let store = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()?
            .OpenPropertyStore(STGM_READ)
            .ok()?;
        let mut value = store.GetValue(&CONTAINER).ok()?;
        let id = PropVariantToGUID(&value);
        let _ = PropVariantClear(&mut value);
        id.ok().map(|id| format!("{id:?}"))
    }
}

/// The optional buttons the user has turned on in Windows Settings →
/// Personalization → Taskbar, so the menu bar offers the same ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskbarButtons {
    pub search: bool,
    pub task_view: bool,
    pub widgets: bool,
}

const ADVANCED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
const SEARCH: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Search");

fn dword(key: PCWSTR, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` and `size` live for the call and match the requested type.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(data)
}

/// Alt+Tab also lists Microsoft Edge tabs ("Show tabs from apps"), or the
/// windows of every virtual desktop: then its list is not the open windows
/// of this desktop in order.
#[must_use]
pub fn alt_tab_lists_more_than_windows(edge_open: bool) -> bool {
    // Absent means the default: three Edge tabs, this desktop only.
    let tabs = dword(ADVANCED, "MultiTaskingAltTabFilter").unwrap_or(2) != 3;
    let all_desktops = dword(ADVANCED, "VirtualDesktopAltTabFilter").unwrap_or(1) == 0;
    (tabs && edge_open) || all_desktops
}

/// Read when the bar redraws: three registry values, no watch needed. A value
/// Windows has never written means the button is at its default, shown.
#[must_use]
pub fn taskbar_buttons() -> TaskbarButtons {
    TaskbarButtons {
        search: dword(SEARCH, "SearchboxTaskbarMode").unwrap_or(1) != 0,
        task_view: dword(ADVANCED, "ShowTaskViewButton").unwrap_or(1) != 0,
        // Windows 10 has no Widgets (its value is never written there, so it
        // would read as shown), and Win+W opens Windows Ink instead.
        widgets: crate::system::is_windows_11() && dword(ADVANCED, "TaskbarDa").unwrap_or(1) != 0,
    }
}

/// The user's native taskbar badge preference. An unset value uses Windows'
/// default (enabled); the dock never writes or overrides this preference.
#[must_use]
pub fn taskbar_badges_enabled() -> bool {
    dword(ADVANCED, "TaskbarBadges").unwrap_or(1) != 0
}

/// Windows Settings → System → Power & battery → Battery percentage.
/// Missing or unreadable means off, matching the Windows default. Read-only:
/// the user's preference remains owned by Windows, including on older builds.
#[must_use]
pub fn battery_percentage_enabled() -> bool {
    dword(ADVANCED, "IsBatteryPercentageEnabled").is_some_and(|value| value != 0)
}

/// Reports changes to the user's taskbar preferences, including the battery
/// percentage. Uses registry notifications rather than a polling timer.
#[must_use]
pub fn watch_taskbar_settings(hwnd: isize, message: u32) -> Option<crate::watch::RegistryWatch> {
    crate::watch::RegistryWatch::new(ADVANCED, false, hwnd, message)
}

/* ---------------------------------------------------------------- Privacy */

/// Where Windows records which apps use the microphone and camera, and
/// when: the source of its own privacy icons in the tray.
pub const CONSENT_STORE: windows::core::PCWSTR = windows::core::w!(
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore"
);

/// Which sensitive devices some app is using right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Privacy {
    pub microphone: bool,
    pub camera: bool,
}

// Location is left out on purpose: Widgets keeps it "in use" all the time
// for the weather, and Windows shows no icon for that.

/// Reads [`CONSENT_STORE`]. Call it when [`watch_privacy`] reports a change,
/// not on a timer.
#[must_use]
pub fn privacy() -> Privacy {
    Privacy {
        microphone: capability_in_use("microphone"),
        camera: capability_in_use("webcam"),
    }
}

/// Tells a window, by posting it `message`, when an app starts or stops
/// using the microphone or camera.
#[must_use]
pub fn watch_privacy(hwnd: isize, message: u32) -> Option<crate::watch::RegistryWatch> {
    crate::watch::RegistryWatch::new(CONSENT_STORE, true, hwnd, message)
}

/// An app is using `capability` while its key has a start time and no stop
/// time. Store apps have a key each; desktop programs sit under `NonPackaged`.
fn capability_in_use(capability: &str) -> bool {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD, RegCloseKey, RegEnumKeyExW,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR, w};

    let in_use = |parent: HKEY, path: &str| -> bool {
        let mut key = HKEY::default();
        // SAFETY: Read-only registry walk; every opened key is closed.
        unsafe {
            if RegOpenKeyExW(parent, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
                return false;
            }
            let mut found = false;
            let mut index = 0;
            loop {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(
                    key,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
                .is_err()
                {
                    break;
                }
                index += 1;
                let read = |value: PCWSTR| {
                    let mut time = 0u64;
                    let mut size = size_of::<u64>() as u32;
                    RegGetValueW(
                        key,
                        PCWSTR(name.as_ptr()),
                        value,
                        RRF_RT_REG_QWORD,
                        None,
                        Some((&raw mut time).cast()),
                        Some(&mut size),
                    )
                    .is_ok()
                    .then_some(time)
                };
                if read(w!("LastUsedTimeStart")).is_some_and(|start| start != 0)
                    && read(w!("LastUsedTimeStop")) == Some(0)
                {
                    found = true;
                    break;
                }
            }
            let _ = RegCloseKey(key);
            found
        }
    };
    let base = format!(
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{capability}"
    );
    in_use(HKEY_CURRENT_USER, &base) || in_use(HKEY_CURRENT_USER, &format!(r"{base}\NonPackaged"))
}
