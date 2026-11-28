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
        // Only the unique temporary directory this test successfully created.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn registered_chrome_web_apps_keep_identity_when_the_shortcut_targets_chrome()
-> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let folder = env::temp_dir().join(format!("muelle-web-app-{}-{unique}", std::process::id()));
    fs::create_dir(&folder)?;
    let folder = Folder(folder);
    // An existing path for the real shell property handler, never an executable
    // or a browser installation. No command in this fixture is executed.
    let host = folder.0.join("chrome.exe");
    fs::create_dir(&host)?;
    for (name, arguments, separate) in [
        ("browser", "--profile-directory=Default", false),
        (
            "web-app",
            "--profile-directory=Default --app-id=abcdefghijklmnopabcdefghijklmnop",
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
        // The property handler expands short TEMP paths on GitHub's runner;
        // compare the same spelling that process-image lookup would return.
        let program = unsafe { take_string(item.GetString(&PKEY_Link_TargetParsingPath)?) }
            .ok_or("missing native shortcut target")?;
        assert!(program.to_ascii_lowercase().ends_with("chrome.exe"));
        assert_eq!(separate_item(&item, &program), separate, "{name}");
    }
    Ok(())
}
