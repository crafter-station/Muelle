//! Issue #72: Firefox web apps open the browser itself, unlike Chromium's
//! proxies, and must still be apps of their own.
use super::*;
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Balanced successful initialization on this test thread.
        unsafe { CoUninitialize() };
    }
}
struct Folder(PathBuf);
impl Drop for Folder {
    fn drop(&mut self) {
        // Only the unique temporary directory this test created.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn firefox_web_apps_are_apps_of_their_own_but_its_tabs_and_profiles_are_not()
-> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let folder = env::temp_dir().join(format!("muelle-firefox-{}-{unique}", std::process::id()));
    fs::create_dir(&folder)?;
    let folder = Folder(folder);
    // An existing path for the shell's property handler; never run.
    let host = folder.0.join("firefox.exe");
    fs::create_dir(&host)?;
    for (name, arguments, separate) in [
        ("firefox", "", false),
        ("profile", r#"-P "Work" -no-remote"#, false),
        ("private", "-private-window", false),
        (
            "web-app",
            r#"-taskbar-tab {0b6e3c55-4a1c-4b51-9a5d-3f2b7d1c9e10} -new-window "https://www.instagram.com/" -profile "C:\Profiles\abc.default""#,
            true,
        ),
    ] {
        let path = folder.0.join(format!("{name}.lnk"));
        let item: IShellItem2 = unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(host.as_os_str()))?;
            link.SetArguments(&HSTRING::from(arguments))?;
            link.cast::<IPersistFile>()?
                .Save(&HSTRING::from(path.as_os_str()), true)?;
            SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?
        };
        let program = unsafe { take_string(item.GetString(&PKEY_Link_TargetParsingPath)?) }
            .ok_or("missing native shortcut target")?;
        assert_eq!(separate_item(&item, &program), separate, "{name}");
    }
    Ok(())
}

#[test]
fn only_a_taskbar_tab_with_its_id_makes_a_web_app() {
    use crate::browser_profiles::{is_gecko, taskbar_tab_arguments};
    assert!(taskbar_tab_arguments(
        "-taskbar-tab abc -new-window https://x.test"
    ));
    assert!(taskbar_tab_arguments("--taskbar-tab abc"));
    assert!(!taskbar_tab_arguments("-taskbar-tab"));
    assert!(!taskbar_tab_arguments(
        "-taskbar-tab -new-window https://x.test"
    ));
    assert!(!taskbar_tab_arguments("-new-window https://x.test"));
    assert!(!taskbar_tab_arguments(""));
    assert!(is_gecko(r"C:\Program Files\Mozilla Firefox\firefox.exe"));
    assert!(is_gecko(r"c:\program files\librewolf\LIBREWOLF.EXE"));
    assert!(!is_gecko(
        r"C:\Program Files\Google\Chrome\Application\chrome.exe"
    ));
    assert!(!is_gecko(
        r"C:\Program Files\Mozilla Firefox\private_browsing.exe"
    ));
}
