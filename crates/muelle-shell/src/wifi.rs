//! Wi-Fi for the menu bar: the networks in range, connecting to a known one,
//! and the radio switch.
//!
//! Only networks Windows already has a profile for can be joined from here;
//! asking for a password is left to the Windows Wi-Fi flyout.

use crate::radio::{RadioError, RadioKind};
use std::{cmp::Reverse, collections::HashSet};
use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, HANDLE},
        NetworkManagement::WiFi::{
            WLAN_AVAILABLE_NETWORK_CONNECTED, WLAN_AVAILABLE_NETWORK_HAS_PROFILE,
            WLAN_AVAILABLE_NETWORK_INCLUDE_ALL_MANUAL_HIDDEN_PROFILES, WLAN_AVAILABLE_NETWORK_LIST,
            WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST,
            WlanCloseHandle, WlanConnect, WlanEnumInterfaces, WlanFreeMemory,
            WlanGetAvailableNetworkList, WlanOpenHandle, WlanScan, dot11_BSS_type_any,
            wlan_connection_mode_profile,
        },
    },
    core::{GUID, HSTRING, PCWSTR},
};

#[derive(Debug, thiserror::Error)]
pub enum WifiError {
    #[error("La red no tiene un perfil guardado en Windows")]
    NoProfile,
    #[error("El servicio Wi-Fi de Windows no responde (codigo {0})")]
    Wlan(u32),
    #[error("No hay un adaptador Wi-Fi")]
    NoRadio,
    #[error("Windows no permite cambiar el Wi-Fi")]
    Denied,
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 0 to 100, as Windows reports it.
    pub signal_quality: u8,
    pub secured: bool,
    pub connected: bool,
    /// Windows has a saved profile, so [`connect`] needs no password.
    pub has_profile: bool,
    /// Saved profile to join with; empty without one.
    pub profile: String,
    /// Adapter the network was seen on.
    pub interface: GUID,
}

/// An open WLAN client handle, closed on drop.
struct Client(HANDLE);

impl Client {
    fn open() -> Option<Self> {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        // SAFETY: Both out pointers live for the call; the handle is closed
        // in `Drop` only when the call succeeded.
        let status = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        (status == ERROR_SUCCESS.0).then_some(Self(handle))
    }

    /// The Wi-Fi adapters, empty when there are none.
    fn interfaces(&self) -> Vec<WLAN_INTERFACE_INFO> {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: On success `list` points to `dwNumberOfItems` entries owned
        // by Windows; they are copied out and the block freed right after.
        unsafe {
            if WlanEnumInterfaces(self.0, None, &mut list) != ERROR_SUCCESS.0 || list.is_null() {
                return Vec::new();
            }
            let items = std::slice::from_raw_parts(
                (*list).InterfaceInfo.as_ptr(),
                (*list).dwNumberOfItems as usize,
            )
            .to_vec();
            WlanFreeMemory(list.cast_const().cast());
            items
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: Closes the handle opened in `open`.
        unsafe {
            WlanCloseHandle(self.0, None);
        }
    }
}

/// Reads a null-terminated UTF-16 buffer.
fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// The networks in range, connected and strongest first, one per name.
/// Empty without an adapter or while the Wi-Fi service is off.
#[must_use]
pub fn networks() -> Vec<WifiNetwork> {
    let Some(client) = Client::open() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for interface in client.interfaces() {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        // SAFETY: On success `list` points to `dwNumberOfItems` entries owned
        // by Windows; they are read before the block is freed.
        unsafe {
            if WlanGetAvailableNetworkList(
                client.0,
                &interface.InterfaceGuid,
                WLAN_AVAILABLE_NETWORK_INCLUDE_ALL_MANUAL_HIDDEN_PROFILES,
                None,
                &mut list,
            ) != ERROR_SUCCESS.0
                || list.is_null()
            {
                continue;
            }
            let items = std::slice::from_raw_parts(
                (*list).Network.as_ptr(),
                (*list).dwNumberOfItems as usize,
            );
            for item in items {
                let length = (item.dot11Ssid.uSSIDLength as usize).min(32);
                let ssid = String::from_utf8_lossy(&item.dot11Ssid.ucSSID[..length]).into_owned();
                if ssid.is_empty() {
                    continue;
                }
                let has_profile = item.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0;
                found.push(WifiNetwork {
                    ssid,
                    signal_quality: item.wlanSignalQuality.min(100) as u8,
                    secured: item.bSecurityEnabled.as_bool(),
                    connected: item.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    has_profile,
                    profile: if has_profile {
                        from_wide(&item.strProfileName)
                    } else {
                        String::new()
                    },
                    interface: interface.InterfaceGuid,
                });
            }
            WlanFreeMemory(list.cast_const().cast());
        }
    }
    tidy(found)
}

/// One entry per name: the connected one, else one with a profile, else the
/// strongest. Connected first, then by signal, then by name.
fn tidy(list: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    let mut list = list;
    list.sort_by(|a, b| {
        (
            Reverse(a.connected),
            Reverse(a.has_profile),
            Reverse(a.signal_quality),
            &a.ssid,
        )
            .cmp(&(
                Reverse(b.connected),
                Reverse(b.has_profile),
                Reverse(b.signal_quality),
                &b.ssid,
            ))
    });
    let mut seen = HashSet::new();
    list.retain(|network| seen.insert(network.ssid.clone()));
    list.sort_by(|a, b| {
        (Reverse(a.connected), Reverse(a.signal_quality), &a.ssid).cmp(&(
            Reverse(b.connected),
            Reverse(b.signal_quality),
            &b.ssid,
        ))
    });
    list
}

/// Asks every adapter to look for networks again; the answer shows up in
/// [`networks`] a few seconds later. Errors are ignored.
pub fn scan() {
    let Some(client) = Client::open() else {
        return;
    };
    for interface in client.interfaces() {
        // SAFETY: The GUID outlives the call; the optional arguments are unused.
        unsafe {
            WlanScan(client.0, &interface.InterfaceGuid, None, None, None);
        }
    }
}

/// Joins a network Windows has a profile for. Returns once Windows accepts
/// the request, not once the connection is up.
///
/// # Errors
/// [`WifiError::NoProfile`] without a saved profile, or the code Windows gave.
pub fn connect(network: &WifiNetwork) -> Result<(), WifiError> {
    if !network.has_profile || network.profile.is_empty() {
        return Err(WifiError::NoProfile);
    }
    let client = Client::open().ok_or(WifiError::Wlan(1062))?;
    let profile = HSTRING::from(network.profile.as_str());
    let parameters = WLAN_CONNECTION_PARAMETERS {
        wlanConnectionMode: wlan_connection_mode_profile,
        strProfile: PCWSTR(profile.as_ptr()),
        pDot11Ssid: std::ptr::null_mut(),
        pDesiredBssidList: std::ptr::null_mut(),
        dot11BssType: dot11_BSS_type_any,
        dwFlags: 0,
    };
    // SAFETY: `parameters` and the profile string live for the call.
    let status = unsafe { WlanConnect(client.0, &network.interface, &parameters, None) };
    if status == ERROR_SUCCESS.0 {
        Ok(())
    } else {
        Err(WifiError::Wlan(status))
    }
}

/// Whether the Wi-Fi radio is on; `None` without one. Blocks on Windows
/// while it lists the radios: call it from a worker thread.
#[must_use]
pub fn radio_on() -> Option<bool> {
    crate::radio::radio_on(RadioKind::WiFi)
}

/// Turns the Wi-Fi radio on or off. Blocks like [`radio_on`].
///
/// # Errors
/// [`WifiError::NoRadio`], [`WifiError::Denied`], or the Windows error.
pub fn set_radio(on: bool) -> Result<(), WifiError> {
    crate::radio::set_radio(RadioKind::WiFi, on).map_err(|error| match error {
        RadioError::NoRadio => WifiError::NoRadio,
        RadioError::Denied => WifiError::Denied,
        RadioError::Windows(error) => WifiError::Windows(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, signal: u8, connected: bool, has_profile: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal_quality: signal,
            secured: true,
            connected,
            has_profile,
            profile: if has_profile {
                ssid.into()
            } else {
                String::new()
            },
            interface: GUID::zeroed(),
        }
    }

    #[test]
    fn dedupes_keeping_connected() {
        let out = tidy(vec![
            network("Casa", 90, false, true),
            network("Casa", 20, true, true),
        ]);
        assert_eq!(out.len(), 1);
        assert!(out[0].connected);
        assert_eq!(out[0].signal_quality, 20);
    }

    #[test]
    fn dedupes_preferring_profile_then_signal() {
        let out = tidy(vec![
            network("Cafe", 90, false, false),
            network("Cafe", 40, false, true),
        ]);
        assert_eq!(out.len(), 1);
        assert!(out[0].has_profile);

        let out = tidy(vec![
            network("Cafe", 30, false, false),
            network("Cafe", 80, false, false),
        ]);
        assert_eq!(out[0].signal_quality, 80);
    }

    #[test]
    fn sorts_connected_then_signal_then_name() {
        let out = tidy(vec![
            network("b", 50, false, false),
            network("a", 50, false, false),
            network("c", 90, false, false),
            network("z", 10, true, true),
        ]);
        let names: Vec<_> = out.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["z", "c", "a", "b"]);
    }

    #[test]
    fn empty_stays_empty() {
        assert!(tidy(Vec::new()).is_empty());
    }
}
