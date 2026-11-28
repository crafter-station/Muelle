//! Explicit desktop integration test. Only its own temporary icon/preference
//! is touched; no real application is invoked and the taskbar stays hidden.
use super::*;
use std::sync::atomic::AtomicU32;
use windows::{
    Win32::{
        Foundation::LRESULT,
        System::{LibraryLoader::GetModuleHandleW, Registry::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{GUID, PCWSTR, w},
};
static CLICKS: AtomicU32 = AtomicU32::new(0);
static REGISTRY_DIRTY: AtomicBool = AtomicBool::new(false);
const CALLBACK: u32 = WM_APP + 113;
const REGISTRY_CALLBACK: u32 = WM_APP + 114;
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == REGISTRY_CALLBACK {
        REGISTRY_DIRTY.store(true, Ordering::Release);
        return LRESULT(0);
    }
    if msg == CALLBACK {
        let action = l.0 as u32 & 0xffff;
        if action == NIN_SELECT
            || action == (NIN_SELECT | 1) // NINF_KEY: activation through UIA/keyboard.
            || action == WM_LBUTTONUP
            || action == WM_LBUTTONDBLCLK
        {
            CLICKS.fetch_or(1, Ordering::Relaxed);
        }
        if action == WM_CONTEXTMENU || action == WM_RBUTTONUP {
            CLICKS.fetch_or(2, Ordering::Relaxed);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}
struct Fixture {
    data: NOTIFYICONDATAW,
    key: Option<Vec<u16>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only the icon with our newly generated GUID and its own cache key.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            let _ = DestroyWindow(self.data.hWnd);
            if let Some(path) = &self.key {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()));
            }
        }
    }
}
fn pump_until(mut ready: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn pump_registry_until(
    watch: &Watch,
    registry: &mut muelle_shell::watch::RegistryWatch,
    mut ready: impl FnMut() -> bool,
) -> bool {
    pump_until(|| {
        if REGISTRY_DIRTY.swap(false, Ordering::AcqRel) {
            registry.arm();
            watch.refresh();
        }
        ready()
    })
}

#[test]
#[ignore = "creates and removes its own temporary native tray icon on the desktop"]
fn native_tray_default_and_context_actions() -> anyhow::Result<()> {
    run_fixture(true)
}

#[test]
#[ignore = "creates and removes its own temporary legacy tray icon on the desktop"]
fn native_legacy_message_only_owner_identity() -> anyhow::Result<()> {
    run_fixture(false)
}

fn set_visibility(path: &[u16], visible: bool) -> anyhow::Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
        .ok()?;
        let result = RegSetValueExW(
            key,
            w!("IsPromoted"),
            None,
            REG_DWORD,
            Some(&u32::from(visible).to_le_bytes()),
        );
        let _ = RegCloseKey(key);
        result.ok()?;
    }
    Ok(())
}

fn run_fixture(with_guid: bool) -> anyhow::Result<()> {
    println!(
        "native fixture identity: {}",
        if with_guid { "GUID" } else { "HWND + UID" }
    );
    CLICKS.store(0, Ordering::Relaxed);
    let native_taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), None)? };
    let native_visibility =
        unsafe { GetWindowLongPtrW(native_taskbar, GWL_STYLE) } & WS_VISIBLE.0 as isize;
    let mut fixture = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpfnWndProc: Some(procedure),
            lpszClassName: w!("MuelleTrayActionProbe"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("Muelle tray integration probe"),
            WS_OVERLAPPED,
            0,
            0,
            1,
            1,
            if with_guid { None } else { Some(HWND_MESSAGE) },
            None,
            Some(instance.into()),
            None,
        )?;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uFlags: NIF_MESSAGE
                | NIF_ICON
                | NIF_TIP
                | NIF_SHOWTIP
                | if with_guid {
                    NIF_GUID
                } else {
                    NOTIFY_ICON_DATA_FLAGS(0)
                },
            uCallbackMessage: CALLBACK,
            hIcon: LoadIconW(None, IDI_INFORMATION)?,
            guidItem: GUID::new()?,
            ..Default::default()
        };
        data.uID = (data.guidItem.data1 % 30000) + 1;
        if !with_guid {
            data.guidItem = GUID::zeroed();
        }
        let tip: Vec<_> = "Muelle temporary integration probe"
            .encode_utf16()
            .collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            anyhow::bail!("NIM_ADD failed");
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        Fixture { data, key: None }
    };
    if !with_guid {
        // Windows need not publish metadata for a newly registered legacy
        // icon. Exercise its documented live identity, not a fabricated cache.
        let record = registry::Record {
            id: 0,
            guid: None,
            uid: Some(fixture.data.uID),
            executable: Some(std::env::current_exe()?),
        };
        let owners = registry::owners(std::slice::from_ref(&record));
        assert!(
            owners
                .values()
                .flatten()
                .any(|h| *h == fixture.data.hWnd.0 as isize),
            "message-only callback owner not discovered"
        );
        assert!(
            registry::location(&record, &owners).is_some(),
            "live HWND + UID not found by Shell"
        );
        return Ok(());
    }
    let mut record_id = None;
    assert!(
        pump_until(|| {
            record_id = registry::records()
                .iter()
                .find(|r| {
                    if with_guid {
                        r.guid == Some(fixture.data.guidItem)
                    } else {
                        r.uid == Some(fixture.data.uID)
                            && r.executable.as_ref().is_some_and(|p| {
                                std::env::current_exe()
                                    .is_ok_and(|own| registry::same_executable(&own, p))
                            })
                    }
                })
                .map(|r| r.id);
            record_id.is_some()
        }),
        "Windows did not cache the fixture icon (GUID mode: {with_guid})"
    );
    let id = record_id.ok_or_else(|| anyhow::anyhow!("fixture identity missing"))?;
    let path: Vec<_> = format!("Control Panel\\NotifyIconSettings\\{id}\0")
        .encode_utf16()
        .collect();
    fixture.key = Some(path.clone());
    set_visibility(&path, true)?;
    let watch = Watch::new(0, 0).ok_or_else(|| anyhow::anyhow!("worker could not start"))?;
    REGISTRY_DIRTY.store(false, Ordering::Release);
    let mut registry_watch = muelle_shell::watch::RegistryWatch::new(
        registry::PATH,
        true,
        fixture.data.hWnd.0 as isize,
        REGISTRY_CALLBACK,
    )
    .ok_or_else(|| anyhow::anyhow!("registry watch could not start"))?;
    assert!(
        pump_until(|| { watch.snapshot().icons().iter().any(|i| i.id == id) }),
        "fixture not visible in live tray snapshot"
    );
    watch.invoke(id, false);
    assert!(
        pump_until(|| CLICKS.load(Ordering::Relaxed) & 1 != 0),
        "native default action was not delivered"
    );
    watch.invoke(id, true);
    assert!(
        pump_until(|| CLICKS.load(Ordering::Relaxed) & 2 != 0),
        "native context action was not delivered"
    );
    set_visibility(&path, false)?;
    assert!(
        pump_registry_until(&watch, &mut registry_watch, || {
            !watch.snapshot().icons().iter().any(|i| i.id == id)
        }),
        "icon stayed visible after Windows hid it"
    );
    set_visibility(&path, true)?;
    assert!(
        pump_registry_until(&watch, &mut registry_watch, || {
            watch.snapshot().icons().iter().any(|i| i.id == id)
        }),
        "icon did not follow Windows visibility being restored"
    );
    // Removing a live icon must also remove its Muelle representation.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &fixture.data);
    }
    assert!(
        pump_registry_until(&watch, &mut registry_watch, || {
            !watch.snapshot().icons().iter().any(|i| i.id == id)
        }),
        "stale icon retained after NIM_DELETE"
    );
    assert_eq!(
        unsafe { GetWindowLongPtrW(native_taskbar, GWL_STYLE) } & WS_VISIBLE.0 as isize,
        native_visibility,
        "native taskbar visibility was changed"
    );
    Ok(())
}
