use super::*;

const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";

#[test]
fn installed_web_app_arguments_are_distinct_from_profiles_and_url_text() {
    let app = "abcdefghijklmnopabcdefghijklmnop";
    for arguments in [
        format!("--profile-directory=Default --app-id={app}"),
        format!("--profile-directory=\"Profile 1\" \"--app-id={app}\""),
        format!("--app-id={}", app.to_ascii_uppercase()),
        format!(
            "--app-id={app} --profile-directory=Default --app-launch-source=4 --source-shortcut-location=desktop"
        ),
    ] {
        assert!(web_app_arguments(&arguments), "rejected {arguments}");
    }
    for arguments in [
        "--profile-directory=Default".to_owned(),
        format!("https://example.com/?--app-id={app}"),
        format!("--title=--app-id={app}"),
        "--app-id=".into(),
        "--app-id=invalid".into(),
        "--app-id=zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".into(),
        format!("--app-id={app} --app-id={app}"),
        format!("--app-id={app}\0"),
        "x".repeat(MAX_TEXT + 1),
        format!("{} --app-id={app}", "--extra ".repeat(16)),
    ] {
        assert!(!web_app_arguments(&arguments));
    }
}

fn profile(directory: &str) -> Profile {
    Profile {
        executable: CHROME.to_lowercase(),
        aumid: format!("Chrome.{directory}"),
        directory: directory.into(),
        user_data: String::new(),
    }
}

#[test]
fn profile_ids_separate_profiles_and_survive_round_trip() {
    let personal = profile("Default");
    let work = profile("Profile 1");
    assert_ne!(personal.id(), work.id());
    assert_eq!(Profile::parse(&personal.id()), Some(personal));
    assert_eq!(Profile::parse(&work.id()), Some(work));
}

#[test]
fn unicode_and_custom_user_data_relaunch_without_switch_injection() {
    let command = format!(
        "{} --profile-directory={} --user-data-dir={}",
        quote(CHROME),
        quote("Trabajo 日本語"),
        quote("C:\\Profiles & work\\")
    );
    let Some(profile) = Profile::from_command(CHROME, "Chrome.Work", &command) else {
        panic!("valid custom profile rejected")
    };
    let reconstructed = format!("{} {}", quote(&profile.executable), profile.arguments());
    assert_eq!(
        Profile::from_command(&profile.executable, profile.aumid(), &reconstructed),
        Some(profile.clone())
    );
    assert_eq!(Profile::parse(&profile.id()), Some(profile));
}

#[test]
fn desktop_provenance_does_not_change_profile_identity_or_relaunch() {
    let normal = profile("Profile 1");
    let command = format!(
        "{} {} --source-shortcut-location=desktop",
        quote(CHROME),
        normal.arguments()
    );
    let Some(desktop) = Profile::from_command(CHROME, normal.aumid(), &command) else {
        panic!("valid desktop profile rejected")
    };
    assert_eq!(desktop.id(), normal.id());
    assert!(!desktop.arguments().contains("source-shortcut"));
}

#[test]
fn reject_other_programs_web_apps_guest_and_unsafe_launch_destinations() {
    for command in [
        r#""C:\Windows\System32\cmd.exe" --profile-directory=Default"#.to_string(),
        format!(
            "{} --profile-directory=Default --app-id=abcdef",
            quote(CHROME)
        ),
        format!("{} --profile-directory=Default --guest", quote(CHROME)),
        format!("{} --profile-directory=Default --incognito", quote(CHROME)),
        format!(
            "{} --profile-directory=Default --remote-debugging-port=9222",
            quote(CHROME)
        ),
        format!(
            "{} --profile-directory=Default https://example.com",
            quote(CHROME)
        ),
        format!(
            "{} --profile-directory=Default --profile-directory=Other",
            quote(CHROME)
        ),
        format!("{} --profile-directory=../Default", quote(CHROME)),
        format!(
            r#"{} --profile-directory=Default --user-data-dir="\\server\share""#,
            quote(CHROME)
        ),
    ] {
        assert!(
            Profile::from_command(CHROME, "Chrome", &command).is_none(),
            "accepted {command}"
        );
    }
    assert!(
        Profile::from_command(
            r"C:\Browser\firefox.exe",
            "Firefox",
            r#""C:\Browser\firefox.exe" --profile-directory=Default"#
        )
        .is_none()
    );
    assert!(Profile::parse("muelle:browser-profile:zz/00/00/").is_none());
    assert!(icon_location(r"\\server\icon.ico,0").is_none());
    assert!(icon_location(r"C:\icon.ico:stream,0").is_none());
}

#[test]
fn chromium_browsers_use_the_same_public_contract_when_available() {
    for executable in [
        r"C:\Browser\msedge.exe",
        r"C:\Browser\brave.exe",
        r"C:\Browser\vivaldi.exe",
        r"C:\Browser\chromium.exe",
    ] {
        let command = format!("{} --profile-directory=Default", quote(executable));
        let Some(profile) = Profile::from_command(executable, "Browser.Default", &command) else {
            panic!("valid browser profile rejected")
        };
        assert_eq!(
            program(&profile.id()).as_deref(),
            Some(executable.to_lowercase().as_str())
        );
    }
}

#[test]
fn saved_cosmetic_metadata_cannot_select_another_profile() -> Result<(), Box<dyn std::error::Error>>
{
    let folder = std::env::temp_dir().join(format!(
        "muelle-profile-cache-{}-{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&folder)?;
    let id = profile("Isolated cache fixture").id();
    let wrong = Visual {
        id: profile("Other fixture").id(),
        name: "Other".into(),
        icon: "".into(),
    };
    fs::write(visual_path(&id, &folder), serde_json::to_vec(&wrong)?)?;
    assert!(visual(&id, &folder).is_none());
    let correct = Visual {
        id: id.clone(),
        name: "Work".into(),
        icon: "".into(),
    };
    remember(correct.clone());
    assert_eq!(visual(&id, &folder), Some(correct.clone()));
    assert_eq!(
        serde_json::from_slice::<Visual>(&fs::read(visual_path(&id, &folder))?)?,
        correct
    );
    fs::remove_dir_all(folder)?;
    Ok(())
}

#[test]
#[ignore = "Read-only diagnostic of browser windows on the current desktop"]
fn live_browser_profiles_publish_usable_metadata() -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let windows = crate::tracker::WindowTracker::default().list();
    let cache = metadata()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let visuals = cache.visuals.clone();
    drop(cache);
    for family in [
        "Google Chrome",
        "Microsoft Edge",
        "Brave",
        "Vivaldi",
        "Chromium",
    ] {
        let profiles: std::collections::HashSet<_> = windows
            .iter()
            .filter(|window| {
                program(&window.app_id).is_some_and(|path| browser_name(&path) == Some(family))
            })
            .filter_map(|window| Profile::parse(&window.app_id).map(|_| window.app_id.as_str()))
            .collect();
        let with_icon = profiles
            .iter()
            .filter(|id| {
                visuals
                    .get(**id)
                    .and_then(|visual| icon_location(&visual.icon))
                    .is_some_and(|(path, _)| Path::new(path).is_file())
            })
            .count();
        eprintln!(
            "{family}: {} distinct published profiles, {with_icon} existing profile icons",
            profiles.len()
        );
        let separable = profiles
            .iter()
            .filter(|id| {
                let Some(profile) = Profile::parse(id) else {
                    return false;
                };
                let Some((file, index)) = visuals
                    .get(**id)
                    .and_then(|visual| icon_location(&visual.icon))
                else {
                    return false;
                };
                let Ok(published) = crate::icons::load_icon_at(file, index, 256) else {
                    return false;
                };
                let Ok(base) = crate::icons::load_icon_at(&profile.executable, 0, published.width)
                else {
                    return false;
                };
                super::avatar::extract(&published, &base).is_some()
            })
            .count();
        eprintln!("{family}: {separable} profile avatars can be separated safely");
    }
    unsafe {
        CoUninitialize();
    }
    Ok(())
}

#[test]
#[ignore = "Read-only diagnostic of previously published browser icon locations in Muelle's cache"]
fn cached_browser_profile_avatars_can_be_separated() -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let folder = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("local app data")?)
        .join("Muelle/icons");
    for entry in fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("profile-"))
        .take(128)
    {
        let Ok(record) = fs::read(entry.path()) else {
            continue;
        };
        if record.len() > 131_072 {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Visual>(&record) else {
            continue;
        };
        let Some(profile) = Profile::parse(&record.id) else {
            continue;
        };
        let Some((file, index)) = icon_location(&record.icon) else {
            continue;
        };
        let Ok(published) = crate::icons::load_icon_at(file, index, 256) else {
            continue;
        };
        let Ok(base) = crate::icons::load_icon_at(&profile.executable, 0, published.width) else {
            continue;
        };
        let avatar = super::avatar::extract(&published, &base);
        eprintln!(
            "{:?}: published {}x{}, base {}x{}, separable={}",
            browser_name(&profile.executable),
            published.width,
            published.height,
            base.width,
            base.height,
            avatar.is_some()
        );
    }
    unsafe {
        CoUninitialize();
    }
    Ok(())
}

#[test]
fn native_profile_shortcuts_keep_their_identity_after_the_source_is_removed()
-> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, StructuredStorage::PROPVARIANT,
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let folder = std::env::temp_dir().join(format!(
        "muelle-profile-shortcut-{}-{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&folder)?;
    // GitHub's TEMP may contain RUNNER~1. IShellLink::GetPath returns the
    // expanded target, as a running browser does. Prepare the same spelling
    // before deriving the expected ID; keep all identity assertions exact.
    let mut expanded = vec![0u16; MAX_TEXT];
    let count = unsafe {
        windows::Win32::Storage::FileSystem::GetLongPathNameW(
            &HSTRING::from(folder.as_os_str()),
            Some(&mut expanded),
        )
    } as usize;
    if count == 0 || count >= expanded.len() {
        return Err("shortcut fixture's long path unavailable".into());
    }
    let folder = PathBuf::from(String::from_utf16(&expanded[..count])?);
    // Empty fixture is never executed; only its path's existence is checked.
    let executable = folder.join("chrome.exe");
    fs::write(&executable, [])?;
    let source = folder.join("Work profile.lnk");
    let program = executable.to_string_lossy();
    let command = format!(
        "{} --profile-directory={} --source-shortcut-location=desktop",
        quote(&program),
        quote("Profile 1")
    );
    let selected = Profile::from_command(&program, "Chrome.ShortcutFixture", &command)
        .ok_or("shortcut fixture")?;
    let shell: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    unsafe {
        shell.SetPath(&HSTRING::from(executable.as_os_str()))?;
        shell.SetArguments(&HSTRING::from(format!(
            "{} --source-shortcut-location=desktop",
            selected.arguments()
        )))?;
        let store: IPropertyStore = shell.cast()?;
        store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(selected.aumid()))?;
        store.Commit()?;
        shell
            .cast::<IPersistFile>()?
            .Save(&HSTRING::from(source.as_os_str()), true)?;
    }
    assert_eq!(crate::pins::shortcut_id(&source), Some(selected.id()));
    assert_eq!(crate::pins::keep_shortcut(&source), Some(selected.id()));
    fs::remove_file(&source)?;
    let restored = Profile::parse(&selected.id()).ok_or("persisted profile id")?;
    assert_eq!(restored.executable, program.to_lowercase());
    assert_eq!(restored.arguments(), "--profile-directory=\"Profile 1\"");
    assert!(crate::pins::launch_target(&restored.id()).is_none());
    fs::remove_dir_all(folder)?;
    Ok(())
}

#[test]
fn native_window_properties_deliver_separate_names_and_icons_without_explorer()
-> Result<(), Box<dyn std::error::Error>> {
    use windows::{
        Win32::{
            System::Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
                StructuredStorage::PROPVARIANT,
            },
            UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP},
        },
        core::w,
    };
    struct Fixture(HWND);
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    // Only private hidden windows are modified. No browser is opened or altered.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _apartment = Apartment;
    let folder = std::env::temp_dir().join(format!(
        "muelle-profile-native-{}-{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let cache = crate::icon_cache::IconCache::new(folder.clone());
    let icon_file = format!("{}\\System32\\shell32.dll", std::env::var("WINDIR")?);
    let mut ids = Vec::new();
    for (directory, name, index) in [
        ("Native fixture 1", "Personal — Google Chrome", 3),
        ("Native fixture 2", "Work — Google Chrome", 4),
    ] {
        let window = Fixture(unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("Profile fixture"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )?
        });
        let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(window.0)? };
        let selected = profile(directory);
        let command = format!("{} {}", quote(CHROME), selected.arguments());
        for (key, value) in [
            (PKEY_AppUserModel_ID, selected.aumid().to_owned()),
            (PKEY_AppUserModel_RelaunchCommand, command),
            (PKEY_AppUserModel_RelaunchDisplayNameResource, name.into()),
            (
                PKEY_AppUserModel_RelaunchIconResource,
                format!("{icon_file},{index}"),
            ),
        ] {
            unsafe {
                store.SetValue(&key, &PROPVARIANT::from(value.as_str()))?;
            }
        }
        let id = window_id(window.0, CHROME).ok_or("missing public profile properties")?;
        assert!(!window_web_app(window.0));
        let visual = cache.get(&id, 64)?;
        assert_eq!(visual.name, name);
        assert_eq!(
            visual.icon,
            crate::icons::load_icon_at(&icon_file, index, 64)?
        );
        assert_eq!(Profile::parse(&id), Some(selected));
        ids.push(id);
        let app_command = format!(
            "{} --profile-directory=Default --app-id=abcdefghijklmnopabcdefghijklmnop",
            quote(CHROME)
        );
        unsafe {
            store.SetValue(
                &PKEY_AppUserModel_RelaunchCommand,
                &PROPVARIANT::from(app_command.as_str()),
            )?;
        }
        assert!(window_web_app(window.0));
        assert!(
            window_id(window.0, CHROME).is_none(),
            "web apps are never browser profiles"
        );
    }
    assert_ne!(ids[0], ids[1]);
    fs::remove_dir_all(folder)?;
    Ok(())
}
