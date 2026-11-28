use super::*;
use windows::{
    Win32::{
        Storage::EnhancedStorage::PKEY_AppUserModel_RelaunchCommand,
        System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
            StructuredStorage::PROPVARIANT,
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetForegroundWindow, WS_POPUP},
    },
    core::w,
};

#[test]
fn late_browser_properties_refresh_identity_once_without_downgrading_on_missing_data()
-> Result<(), Box<dyn std::error::Error>> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    // Hidden owned window only; never opens a browser or activates user apps.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let window = Window(unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("Muelle late identity fixture"),
            WS_POPUP,
            -32000,
            -32000,
            1,
            1,
            None,
            None,
            None,
            None,
        )?
    });
    let foreground = unsafe { GetForegroundWindow() };
    let program = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
    let hwnd = window.0.0 as isize;
    let mut tracker = WindowTracker::default();
    tracker.app_ids.insert(hwnd, normalize_id(program));
    assert!(!tracker.refresh_browser(hwnd));
    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(window.0)? };
    let aumid = "Chrome.MuelleLateIdentityFixture";
    let command = format!("\"{program}\" --profile-directory=\"Profile 1\"");
    unsafe {
        store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(aumid))?;
        store.SetValue(
            &PKEY_AppUserModel_RelaunchCommand,
            &PROPVARIANT::from(command.as_str()),
        )?;
    }
    let expected = crate::browser_profiles::Profile::from_command(program, aumid, &command)
        .ok_or("missing profile fixture")?
        .id();
    assert!(
        tracker.refresh_browser(hwnd),
        "late identity must request roster reconciliation"
    );
    assert_eq!(tracker.app_ids.get(&hwnd), Some(&expected));
    assert!(
        !tracker.refresh_browser(hwnd),
        "stable identity must not rebuild the roster"
    );
    unsafe {
        store.SetValue(&PKEY_AppUserModel_RelaunchCommand, &PROPVARIANT::default())?;
    }
    assert!(!tracker.refresh_browser(hwnd));
    assert_eq!(tracker.app_ids.get(&hwnd), Some(&expected));
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    Ok(())
}
