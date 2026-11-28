//! The lists behind the control center's tiles: Wi-Fi networks in range,
//! paired Bluetooth devices and the sound outputs.
//!
//! Each call can take a while, so none belongs on a UI thread.

use windows::{
    Devices::{
        Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice},
        Enumeration::DeviceInformation,
    },
    Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation},
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, PROPERTYKEY},
        Media::Audio::{
            DEVICE_STATE_ACTIVE, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
            eCommunications, eConsole, eMultimedia, eRender,
        },
        NetworkManagement::WiFi::{
            WLAN_AVAILABLE_NETWORK_CONNECTED, WLAN_AVAILABLE_NETWORK_LIST,
            WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_INFO_LIST,
            WlanCloseHandle, WlanConnect, WlanEnumInterfaces, WlanFreeMemory,
            WlanGetAvailableNetworkList, WlanGetProfileList, WlanOpenHandle, WlanScan,
            dot11_BSS_type_any, wlan_connection_mode_profile,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ},
    },
    core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, Result, interface},
};

/* ------------------------------------------------------------------ Wi-Fi */

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub name: String,
    /// From 0 to 100; `None` where Windows does not say.
    pub signal: Option<u8>,
    pub connected: bool,
    /// The saved profile that joins it without asking anything.
    pub profile: Option<String>,
    pub secured: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiList {
    /// Connected first, then saved ones, then by signal.
    Networks(Vec<WifiNetwork>),
    /// Windows 11 hides the networks in range from every app while location
    /// access is off. The saved ones, and the one in use, are still known.
    SavedOnly(Vec<WifiNetwork>),
    /// No Wi-Fi adapter, or its service is not running.
    Unavailable,
}

/// A session with the WLAN service, closed when dropped.
struct Wlan {
    handle: HANDLE,
    interfaces: Vec<GUID>,
}

impl Wlan {
    fn open() -> Option<Self> {
        let mut version = 0;
        let mut handle = HANDLE::default();
        // SAFETY: Valid outputs; the handle is closed by `Drop`.
        if unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) } != ERROR_SUCCESS.0 {
            return None;
        }
        let mut wlan = Self {
            handle,
            interfaces: Vec::new(),
        };
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: On success Windows allocates `list`, with
        // `dwNumberOfItems` entries, which is freed here after copying.
        unsafe {
            if WlanEnumInterfaces(wlan.handle, None, &mut list) != ERROR_SUCCESS.0 {
                return None;
            }
            let count = (*list).dwNumberOfItems as usize;
            let first = (*list).InterfaceInfo.as_ptr();
            wlan.interfaces = std::slice::from_raw_parts(first, count)
                .iter()
                .map(|info| info.InterfaceGuid)
                .collect();
            WlanFreeMemory(list.cast());
        }
        (!wlan.interfaces.is_empty()).then_some(wlan)
    }
}

impl Wlan {
    /// The names of the saved networks, which need no location access.
    fn profiles(&self) -> Vec<String> {
        let mut names = Vec::new();
        for interface in &self.interfaces {
            let mut list: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
            // SAFETY: On success Windows allocates `list`, with
            // `dwNumberOfItems` entries, which is freed here after copying.
            unsafe {
                if WlanGetProfileList(self.handle, interface, None, &mut list) != ERROR_SUCCESS.0 {
                    continue;
                }
                let count = (*list).dwNumberOfItems as usize;
                let first = (*list).ProfileInfo.as_ptr();
                names.extend(
                    std::slice::from_raw_parts(first, count)
                        .iter()
                        .map(|profile| utf16_text(&profile.strProfileName)),
                );
                WlanFreeMemory(list.cast());
            }
        }
        names
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        // SAFETY: Closes the handle opened in `open`, once.
        unsafe {
            let _ = WlanCloseHandle(self.handle, None);
        }
    }
}

fn utf16_text(units: &[u16]) -> String {
    let length = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..length])
}

/// The networks Windows currently knows to be in range.
#[must_use]
pub fn wifi_networks() -> WifiList {
    let Some(wlan) = Wlan::open() else {
        return WifiList::Unavailable;
    };
    let mut found = Vec::new();
    for interface in &wlan.interfaces {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        // SAFETY: On success Windows allocates `list`, with
        // `dwNumberOfItems` entries, which is freed here after copying.
        unsafe {
            let status = WlanGetAvailableNetworkList(wlan.handle, interface, 0, None, &mut list);
            if status == ERROR_ACCESS_DENIED.0 {
                return WifiList::SavedOnly(saved_networks(wlan.profiles(), connected_wifi()));
            }
            if status != ERROR_SUCCESS.0 {
                continue;
            }
            let count = (*list).dwNumberOfItems as usize;
            let first = (*list).Network.as_ptr();
            for network in std::slice::from_raw_parts(first, count) {
                let ssid = &network.dot11Ssid;
                let length = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
                let profile = utf16_text(&network.strProfileName);
                found.push(WifiNetwork {
                    name: String::from_utf8_lossy(&ssid.ucSSID[..length]).into_owned(),
                    signal: Some(network.wlanSignalQuality.min(100) as u8),
                    connected: network.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    profile: (!profile.is_empty()).then_some(profile),
                    secured: network.bSecurityEnabled.as_bool(),
                });
            }
            WlanFreeMemory(list.cast());
        }
    }
    WifiList::Networks(arrange(found))
}

/// The Wi-Fi connections in use: name and signal. Unlike the list of what is
/// in range, Windows gives these without location access.
fn connected_wifi() -> Vec<(String, Option<u8>)> {
    let Ok(profiles) = NetworkInformation::GetConnectionProfiles() else {
        return Vec::new();
    };
    profiles
        .into_iter()
        .filter(|profile| {
            profile.IsWlanConnectionProfile().unwrap_or(false)
                && profile
                    .GetNetworkConnectivityLevel()
                    .is_ok_and(|level| level != NetworkConnectivityLevel::None)
        })
        .filter_map(|profile| {
            let name = profile.ProfileName().ok()?.to_string();
            // Windows reports 0 to 5 bars.
            let signal = profile
                .GetSignalBars()
                .and_then(|bars| bars.Value())
                .ok()
                .map(|bars| bars.min(5) * 20);
            Some((name, signal))
        })
        .collect()
}

/// The saved networks, with those in use marked.
fn saved_networks(profiles: Vec<String>, connected: Vec<(String, Option<u8>)>) -> Vec<WifiNetwork> {
    let mut networks: Vec<WifiNetwork> = profiles
        .into_iter()
        .map(|name| WifiNetwork {
            profile: Some(name.clone()),
            name,
            signal: None,
            connected: false,
            secured: true,
        })
        .collect();
    for (name, signal) in connected {
        let known = |wanted: &str| networks.iter().position(|n| n.name == wanted);
        // Windows may number a connection ("Home 2"); its profile keeps the
        // network's own name.
        match known(&name).or_else(|| known(unnumbered(&name))) {
            Some(index) => {
                networks[index].connected = true;
                networks[index].signal = signal;
            }
            None => networks.push(WifiNetwork {
                name,
                signal,
                connected: true,
                profile: None,
                secured: true,
            }),
        }
    }
    arrange(networks)
}

fn unnumbered(name: &str) -> &str {
    name.rsplit_once(' ')
        .filter(|(_, number)| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .map_or(name, |(base, _)| base)
}

/// One entry per name (Windows lists a saved network twice, and each band
/// separately), the useful ones first. Hidden networks have no name to show.
fn arrange(found: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    let mut networks: Vec<WifiNetwork> = Vec::new();
    for network in found.into_iter().filter(|n| !n.name.trim().is_empty()) {
        match networks.iter_mut().find(|n| n.name == network.name) {
            Some(known) => {
                known.signal = known.signal.max(network.signal);
                known.connected |= network.connected;
                known.profile = known.profile.take().or(network.profile);
            }
            None => networks.push(network),
        }
    }
    networks.sort_by_key(|n| {
        (
            !n.connected,
            n.profile.is_none(),
            100 - n.signal.unwrap_or(0),
            n.name.clone(),
        )
    });
    networks
}

/// Asks the adapters to look again; results arrive a few seconds later.
pub fn wifi_scan() {
    if let Some(wlan) = Wlan::open() {
        for interface in &wlan.interfaces {
            // SAFETY: A live handle and one of its interfaces.
            unsafe {
                let _ = WlanScan(wlan.handle, interface, None, None, None);
            }
        }
    }
}

/// Joins a network with its saved profile, as picking it in Windows does.
/// `false` if no adapter accepted the request.
#[must_use]
pub fn wifi_connect(profile: &str) -> bool {
    let Some(wlan) = Wlan::open() else {
        return false;
    };
    let name = HSTRING::from(profile);
    let parameters = WLAN_CONNECTION_PARAMETERS {
        wlanConnectionMode: wlan_connection_mode_profile,
        strProfile: PCWSTR(name.as_ptr()),
        dot11BssType: dot11_BSS_type_any,
        ..Default::default()
    };
    wlan.interfaces.iter().any(|interface| {
        // SAFETY: `parameters` and the name it points to outlive the call.
        let status = unsafe { WlanConnect(wlan.handle, interface, &parameters, None) };
        status == ERROR_SUCCESS.0
    })
}

/* -------------------------------------------------------------- Bluetooth */

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothEntry {
    pub name: String,
    pub connected: bool,
}

/// Paired devices, the connected ones first.
#[must_use]
pub fn paired_bluetooth() -> Vec<BluetoothEntry> {
    let mut found = Vec::new();
    for low_energy in [false, true] {
        let _ = collect_bluetooth(low_energy, &mut found);
    }
    found.sort_by(|a, b| (!a.connected, &a.name).cmp(&(!b.connected, &b.name)));
    found
}

fn collect_bluetooth(low_energy: bool, found: &mut Vec<BluetoothEntry>) -> Result<()> {
    let selector = if low_energy {
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?
    } else {
        BluetoothDevice::GetDeviceSelectorFromPairingState(true)?
    };
    for info in DeviceInformation::FindAllAsyncAqsFilter(&selector)?.join()? {
        let name = info.Name()?.to_string();
        if name.trim().is_empty() {
            continue;
        }
        let id = info.Id()?;
        let status = if low_energy {
            BluetoothLEDevice::FromIdAsync(&id)?
                .join()
                .and_then(|device| device.ConnectionStatus())
        } else {
            BluetoothDevice::FromIdAsync(&id)?
                .join()
                .and_then(|device| device.ConnectionStatus())
        };
        let connected = status.is_ok_and(|s| s == BluetoothConnectionStatus::Connected);
        // A device that speaks both kinds is listed once.
        match found.iter_mut().find(|entry| entry.name == name) {
            Some(entry) => entry.connected |= connected,
            None => found.push(BluetoothEntry { name, connected }),
        }
    }
    Ok(())
}

/* ----------------------------------------------------------- Sound output */

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOutput {
    pub id: String,
    pub name: String,
    /// Where sound plays now.
    pub selected: bool,
}

/// `PKEY_Device_FriendlyName`: "Speakers (Realtek Audio)".
const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: The returned string is copied, then freed as the API requires.
    unsafe {
        let raw = device.GetId().ok()?;
        let id = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0.cast()));
        id
    }
}

/// The connected sound outputs. COM must be initialized on this thread.
#[must_use]
pub fn audio_outputs() -> Vec<AudioOutput> {
    let read = || -> Result<Vec<AudioOutput>> {
        // SAFETY: Plain queries on COM objects created and released here.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let current = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|device| device_id(&device));
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut outputs = Vec::new();
            for index in 0..devices.GetCount()? {
                let device = devices.Item(index)?;
                let Some(id) = device_id(&device) else {
                    continue;
                };
                let name = device
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&FRIENDLY_NAME))
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                if !name.is_empty() {
                    let selected = current.as_ref() == Some(&id);
                    outputs.push(AudioOutput { id, name, selected });
                }
            }
            Ok(outputs)
        }
    };
    read().unwrap_or_default()
}

/// Windows has no documented call to change the output. This is the interface
/// its own sound settings use, unchanged since Windows 7; only
/// `SetDefaultEndpoint` is called, and the methods before it hold its place.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn get_mix_format(&self) -> HRESULT;
    fn get_device_format(&self) -> HRESULT;
    fn reset_device_format(&self) -> HRESULT;
    fn set_device_format(&self) -> HRESULT;
    fn get_processing_period(&self) -> HRESULT;
    fn set_processing_period(&self) -> HRESULT;
    fn get_share_mode(&self) -> HRESULT;
    fn set_share_mode(&self) -> HRESULT;
    fn get_property_value(&self) -> HRESULT;
    fn set_property_value(&self) -> HRESULT;
    fn set_default_endpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
}

const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Makes `id` the output for everything, calls included. COM must be
/// initialized on this thread.
///
/// # Errors
/// If this version of Windows refuses the request.
pub fn select_audio_output(id: &str) -> Result<()> {
    let id = HSTRING::from(id);
    // SAFETY: The interface matches the class's layout up to the one method
    // called, and the string outlives the calls.
    unsafe {
        let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
        for role in [eConsole, eMultimedia, eCommunications] {
            policy
                .set_default_endpoint(PCWSTR(id.as_ptr()), role)
                .ok()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(name: &str, signal: u8, connected: bool, saved: bool) -> WifiNetwork {
        WifiNetwork {
            name: name.into(),
            signal: Some(signal),
            connected,
            profile: saved.then(|| name.to_owned()),
            secured: true,
        }
    }

    #[test]
    fn networks_are_listed_once_with_the_useful_ones_first() {
        let list = arrange(vec![
            network("Cafe", 90, false, false),
            network("", 100, false, false),
            network("Office", 40, false, true),
            network("Home", 55, false, false),
            network("Home", 70, true, true),
            network("Annex", 60, false, false),
        ]);
        let names: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Home", "Office", "Cafe", "Annex"]);
        assert_eq!((list[0].signal, list[0].connected), (Some(70), true));
        assert_eq!(list[0].profile.as_deref(), Some("Home"));
    }

    /// Writes each role's current output back to it, which exercises the
    /// undocumented interface while leaving the sound settings as they were.
    #[test]
    #[ignore = "Calls the sound policy interface with this PC's current outputs"]
    fn native_reselecting_current_outputs_changes_nothing() -> Result<()> {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        // SAFETY: This test thread's own apartment and live COM objects; each
        // role is given the device it already has.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
            for role in [eConsole, eMultimedia, eCommunications] {
                let current = || {
                    enumerator
                        .GetDefaultAudioEndpoint(eRender, role)
                        .ok()
                        .and_then(|device| device_id(&device))
                };
                let Some(before) = current() else {
                    continue;
                };
                let id = HSTRING::from(before.as_str());
                policy
                    .set_default_endpoint(PCWSTR(id.as_ptr()), role)
                    .ok()?;
                assert_eq!(current(), Some(before));
            }
        }
        Ok(())
    }

    #[test]
    fn without_location_the_saved_networks_and_the_one_in_use_are_listed() {
        let saved = vec!["Office".to_owned(), "Home".to_owned()];
        let list = saved_networks(saved.clone(), vec![("Home 2".to_owned(), Some(80))]);
        let names: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Home", "Office"]);
        assert_eq!((list[0].connected, list[0].signal), (true, Some(80)));
        assert_eq!((list[1].connected, list[1].signal), (false, None));
        assert_eq!(list[1].profile.as_deref(), Some("Office"));
        // A connection with no saved profile is still shown.
        let list = saved_networks(saved, vec![("Guest".to_owned(), None)]);
        assert_eq!(list[0].name, "Guest");
        assert!(list[0].connected && list[0].profile.is_none());
        assert_eq!(list.len(), 3);
        assert_eq!(unnumbered("Flat 12b"), "Flat 12b");
    }

    #[test]
    #[ignore = "Read-only queries of this PC's networks, devices and sound outputs"]
    fn native_lists_report_this_pc() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        // SAFETY: This test thread's own apartment.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        // A fresh look, as the control center takes when the menu opens.
        wifi_scan();
        std::thread::sleep(std::time::Duration::from_secs(5));
        match wifi_networks() {
            WifiList::Networks(list) => println!(
                "Wi-Fi: {} networks, {} connected, {} saved",
                list.len(),
                list.iter().filter(|n| n.connected).count(),
                list.iter().filter(|n| n.profile.is_some()).count()
            ),
            WifiList::SavedOnly(list) => println!(
                "Wi-Fi: saved only, {} listed, {} connected, {} joinable",
                list.len(),
                list.iter().filter(|n| n.connected).count(),
                list.iter()
                    .filter(|n| !n.connected && n.profile.is_some())
                    .count()
            ),
            WifiList::Unavailable => println!("Wi-Fi: unavailable"),
        }
        println!("Bluetooth: {} paired", paired_bluetooth().len());
        for output in audio_outputs() {
            println!("output: {} (selected {})", output.name, output.selected);
        }
    }
}
