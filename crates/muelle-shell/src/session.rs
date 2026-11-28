//! Locking, sleeping, restarting, shutting down and signing out, as the Start
//! menu does. Apps are asked to close, never forced, so each can still offer
//! to save.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::{
    Win32::{
        Foundation::{ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
            TOKEN_QUERY,
        },
        System::{
            Power::SetSuspendState,
            Shutdown::{
                EWX_HYBRID_SHUTDOWN, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN,
                EXIT_WINDOWS_FLAGS, ExitWindowsEx, LockWorkStation, SHTDN_REASON_FLAG_PLANNED,
                SHTDN_REASON_MAJOR_OTHER, SHUTDOWN_REASON,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::{Error, Result},
};

/// Shows the lock screen.
pub fn lock() -> Result<()> {
    // SAFETY: Takes no arguments.
    unsafe { LockWorkStation() }
}

/// Puts the PC to sleep. Returns once it wakes.
pub fn sleep() -> Result<()> {
    allow_shutdown()?;
    // SAFETY: Scalar arguments only: sleep rather than hibernate, letting
    // programs that need to finish do so, with wake timers kept.
    if unsafe { SetSuspendState(false, false, false) } {
        Ok(())
    } else {
        Err(Error::from_thread())
    }
}

pub fn restart() -> Result<()> {
    allow_shutdown()?;
    exit(EWX_REBOOT)
}

/// Shuts down, keeping Windows' fast startup where the PC has it.
pub fn shut_down() -> Result<()> {
    allow_shutdown()?;
    let off = EWX_SHUTDOWN | EWX_POWEROFF;
    exit(off | EWX_HYBRID_SHUTDOWN).or_else(|_| exit(off))
}

pub fn sign_out() -> Result<()> {
    exit(EWX_LOGOFF)
}

fn exit(flags: EXIT_WINDOWS_FLAGS) -> Result<()> {
    let reason = SHUTDOWN_REASON(SHTDN_REASON_MAJOR_OTHER.0 | SHTDN_REASON_FLAG_PLANNED.0);
    // SAFETY: Scalar arguments only.
    unsafe { ExitWindowsEx(flags, reason) }
}

/// Turns on this process's right to shut the PC down, which every signed-in
/// user holds but Windows leaves off until asked.
fn allow_shutdown() -> Result<()> {
    let mut raw = HANDLE::default();
    // SAFETY: A pseudo handle for this process and a valid output.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut raw,
        )?;
    }
    // SAFETY: OpenProcessToken transferred one valid, uniquely owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Attributes: SE_PRIVILEGE_ENABLED,
            ..Default::default()
        }],
    };
    // SAFETY: The structure outlives both calls and the token is live.
    unsafe {
        LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut privileges.Privileges[0].Luid)?;
        AdjustTokenPrivileges(
            HANDLE(token.as_raw_handle()),
            false,
            Some(&raw const privileges),
            0,
            None,
            None,
        )?;
        // Success is also reported when nothing was granted.
        if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            return Err(Error::from_hresult(ERROR_NOT_ALL_ASSIGNED.to_hresult()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only turns the right on; nothing is shut down.
    #[test]
    #[ignore = "Needs an account allowed to shut the PC down"]
    fn native_the_shutdown_right_can_be_turned_on() -> Result<()> {
        allow_shutdown()
    }
}
