//! The control center's slow work, on a thread of its own: radios, the
//! monitor's brightness, the theme and the lists behind its tiles.

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use muelle_shell::{
    control_lists::{self, AudioOutput, BluetoothEntry, WifiList},
    controls::{self, RadioSwitch, ScreenBrightness},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    UI::WindowsAndMessaging::PostMessageW,
};

/// A Wi-Fi scan or connection takes about this long to show its result.
const SCAN_WAIT: Duration = Duration::from_millis(2500);
const JOIN_WAIT: Duration = Duration::from_secs(4);

/// What the tiles show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    pub brightness: Option<u8>,
    pub dark: bool,
    /// Do Not Disturb.
    pub focus: Option<bool>,
}

/// What the pages list; `None` until first read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lists {
    pub wifi: Option<WifiList>,
    pub bluetooth: Option<Vec<BluetoothEntry>>,
    pub outputs: Option<Vec<AudioOutput>>,
}

pub enum Command {
    Radio(RadioSwitch, bool),
    Brightness(u8),
    Dark(bool),
    Focus(bool),
    ListWifi,
    JoinWifi(String),
    ListBluetooth,
    ListOutputs,
    SelectOutput(String),
}

#[derive(Default)]
struct Shared {
    readings: Readings,
    lists: Lists,
}

/// The thread ends when this is dropped.
pub struct Worker {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
}

impl Worker {
    /// Posts `message` to `window` after each change.
    pub fn start(window: isize, message: u32) -> Self {
        let (commands, requests) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            readings: Readings {
                dark: controls::dark_mode(),
                ..Readings::default()
            },
            lists: Lists::default(),
        }));
        let report = Report {
            window,
            message,
            shared: shared.clone(),
        };
        let started = thread::Builder::new()
            .name("muelle-controls".into())
            .spawn(move || {
                // SAFETY: A new thread; the apartment outlives its objects.
                if unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok() {
                    work(&report, &requests);
                    // SAFETY: Balances the successful initialization above.
                    unsafe { RoUninitialize() };
                }
            });
        if let Err(error) = started {
            muelle_shell::log::line(&format!("centro de control: {error}"));
        }
        Self { commands, shared }
    }

    pub fn readings(&self) -> Readings {
        self.shared
            .lock()
            .map_or_else(|_| Readings::default(), |shared| shared.readings)
    }

    pub fn lists(&self) -> Lists {
        self.shared
            .lock()
            .map_or_else(|_| Lists::default(), |shared| shared.lists.clone())
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

struct Report {
    window: isize,
    message: u32,
    shared: Arc<Mutex<Shared>>,
}

impl Report {
    fn publish(&self, change: impl FnOnce(&mut Readings, &mut Lists)) {
        if let Ok(mut shared) = self.shared.lock() {
            let Shared { readings, lists } = &mut *shared;
            change(readings, lists);
        }
        // SAFETY: Only posts a message; a window that no longer exists ignores it.
        unsafe {
            let target = Some(HWND(self.window as *mut _));
            let _ = PostMessageW(target, self.message, WPARAM(0), LPARAM(0));
        }
    }

    fn radios(&self) {
        let wifi = controls::radio_on(RadioSwitch::WiFi);
        let bluetooth = controls::radio_on(RadioSwitch::Bluetooth);
        self.publish(|readings, _| {
            readings.wifi = wifi;
            readings.bluetooth = bluetooth;
        });
    }

    fn wifi(&self) {
        let found = control_lists::wifi_networks();
        self.publish(|_, lists| lists.wifi = Some(found));
    }

    fn outputs(&self) {
        let found = control_lists::audio_outputs();
        self.publish(|_, lists| lists.outputs = Some(found));
    }
}

fn work(report: &Report, requests: &Receiver<Command>) {
    report.radios();
    let focus = controls::do_not_disturb();
    report.publish(|readings, _| readings.focus = focus);
    let screen = ScreenBrightness::primary();
    let level = screen.as_ref().and_then(ScreenBrightness::percent);
    report.publish(|readings, _| readings.brightness = level);

    // When to read the Wi-Fi list again.
    let mut rescan: Option<Instant> = None;
    loop {
        let first = match rescan {
            None => match requests.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            },
            Some(at) => match requests.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        if rescan.is_some_and(|at| Instant::now() >= at) {
            rescan = None;
            report.wifi();
        }
        // While a slider moves, only its latest value matters.
        let mut brightness = None;
        for command in first.into_iter().chain(requests.try_iter()) {
            match command {
                Command::Brightness(percent) => brightness = Some(percent),
                Command::Radio(switch, on) => {
                    if let Err(error) = controls::set_radio(switch, on) {
                        muelle_shell::log::line(&format!("centro de control, radio: {error}"));
                    }
                    // A radio takes a moment to report its new state.
                    thread::sleep(Duration::from_millis(300));
                    report.radios();
                    if switch == RadioSwitch::WiFi {
                        rescan = Some(Instant::now() + SCAN_WAIT);
                    }
                }
                Command::Dark(dark) => {
                    if let Err(error) = controls::set_dark_mode(dark) {
                        muelle_shell::log::line(&format!("centro de control, tema: {error}"));
                    }
                    let now = controls::dark_mode();
                    report.publish(|readings, _| readings.dark = now);
                }
                Command::Focus(on) => {
                    if let Err(error) = controls::set_do_not_disturb(on) {
                        muelle_shell::log::line(&format!(
                            "centro de control, no molestar: {error}"
                        ));
                    }
                    let now = controls::do_not_disturb();
                    report.publish(|readings, _| readings.focus = now);
                }
                Command::ListWifi => {
                    // What Windows already knows, then a fresh look.
                    report.wifi();
                    control_lists::wifi_scan();
                    rescan = Some(Instant::now() + SCAN_WAIT);
                }
                Command::JoinWifi(profile) => {
                    if !control_lists::wifi_connect(&profile) {
                        muelle_shell::log::line("centro de control: la red no aceptó la conexión");
                    }
                    rescan = Some(Instant::now() + JOIN_WAIT);
                }
                Command::ListBluetooth => {
                    let found = control_lists::paired_bluetooth();
                    report.publish(|_, lists| lists.bluetooth = Some(found));
                }
                Command::ListOutputs => report.outputs(),
                Command::SelectOutput(id) => {
                    if let Err(error) = control_lists::select_audio_output(&id) {
                        muelle_shell::log::line(&format!("centro de control, salida: {error}"));
                    }
                    report.outputs();
                }
            }
        }
        if let (Some(percent), Some(screen)) = (brightness, &screen) {
            if let Err(error) = screen.set_percent(percent) {
                muelle_shell::log::line(&format!("centro de control, brillo: {error}"));
            }
            report.publish(|readings, _| readings.brightness = Some(percent));
        }
    }
}
