//! The Wi-Fi and Bluetooth radio switches, through `Windows.Devices.Radios`.

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind as WinRadioKind, RadioState};

/// A radio the menu bar can switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioKind {
    WiFi,
    Bluetooth,
}

impl RadioKind {
    const fn native(self) -> WinRadioKind {
        match self {
            Self::WiFi => WinRadioKind::WiFi,
            Self::Bluetooth => WinRadioKind::Bluetooth,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RadioError {
    #[error("No hay un adaptador de radio")]
    NoRadio,
    #[error("Windows no permite cambiar la radio")]
    Denied,
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

fn find(kind: RadioKind) -> Option<Radio> {
    let wanted = kind.native();
    Radio::GetRadiosAsync()
        .ok()?
        .join()
        .ok()?
        .into_iter()
        .find(|radio| radio.Kind().is_ok_and(|found| found == wanted))
}

/// Whether the radio is on; `None` without one. Blocks on Windows while it
/// lists the radios: call it from a worker thread.
#[must_use]
pub fn radio_on(kind: RadioKind) -> Option<bool> {
    Some(find(kind)?.State().ok()? == RadioState::On)
}

/// Whether the Wi-Fi and the Bluetooth radios are on, from a single listing
/// of the radios (`None` for a missing one). Blocks like [`radio_on`].
#[must_use]
pub fn wifi_and_bluetooth_on() -> (Option<bool>, Option<bool>) {
    let Some(radios) = Radio::GetRadiosAsync().ok().and_then(|op| op.join().ok()) else {
        return (None, None);
    };
    let on = |wanted: WinRadioKind| {
        (&radios)
            .into_iter()
            .find(|radio| radio.Kind().is_ok_and(|found| found == wanted))
            .and_then(|radio| radio.State().ok())
            .map(|state| state == RadioState::On)
    };
    (
        on(RadioKind::WiFi.native()),
        on(RadioKind::Bluetooth.native()),
    )
}

/// Turns the radio on or off. Blocks like [`radio_on`]: call it from a worker
/// thread.
///
/// # Errors
/// [`RadioError::NoRadio`], [`RadioError::Denied`], or the Windows error.
pub fn set_radio(kind: RadioKind, on: bool) -> Result<(), RadioError> {
    if Radio::RequestAccessAsync()?.join()? != RadioAccessStatus::Allowed {
        return Err(RadioError::Denied);
    }
    let radio = find(kind).ok_or(RadioError::NoRadio)?;
    let state = if on { RadioState::On } else { RadioState::Off };
    if radio.SetStateAsync(state)?.join()? == RadioAccessStatus::Allowed {
        Ok(())
    } else {
        Err(RadioError::Denied)
    }
}
