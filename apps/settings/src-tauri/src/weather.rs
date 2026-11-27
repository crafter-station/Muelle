//! Foreground, one-shot Windows location. The Dock never borrows Settings'
//! identity/permission: only an approximate fix crosses the config boundary.
use std::{sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}, mpsc}, time::{Duration, Instant}};
use muelle_config::WeatherLocation;
use tauri::State;
use windows::{
    Devices::Geolocation::{Geolocator, GeolocationAccessStatus, PositionAccuracy},
    Foundation::TimeSpan,
    Win32::{Foundation::HWND, System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
        UI::WindowsAndMessaging::GetForegroundWindow},
};
use windows_future::{AsyncStatus, IAsyncOperation};

#[derive(Default)]
pub struct LocationRequest {
    generation: Arc<AtomicU64>,
    busy: Arc<AtomicBool>,
}

struct Busy(Arc<AtomicBool>);
impl Drop for Busy { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }

fn wait<T: windows_core::RuntimeType + 'static>(operation: IAsyncOperation<T>,
    generation: &AtomicU64, expected: u64, timeout: Duration) -> Result<T, String> {
    let until = Instant::now() + timeout;
    loop {
        if generation.load(Ordering::Acquire) != expected || Instant::now() >= until {
            let _ = operation.Cancel();
            return Err(if generation.load(Ordering::Acquire) != expected { "cancelled" } else { "timeout" }.into());
        }
        match operation.Status().map_err(|_| "unavailable")? {
            AsyncStatus::Started => std::thread::sleep(Duration::from_millis(50)),
            _ => return operation.GetResults().map_err(|_| "unavailable".into()),
        }
    }
}

#[tauri::command]
pub async fn locate_weather(window: tauri::WebviewWindow,
    request: State<'_, LocationRequest>) -> Result<WeatherLocation, String> {
    if request.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err("busy".into());
    }
    let guard = Busy(request.busy.clone());
    let expected = request.generation.load(Ordering::Acquire);
    let generation = request.generation.clone();
    let owner = window.hwnd().map_err(|_| "unavailable")?.0 as isize;
    let (send, receive) = mpsc::sync_channel(1);
    // RequestAccessAsync must START on the foreground UI thread. The closure
    // returns immediately; no consent or position wait blocks that thread.
    window.run_on_main_thread(move || {
        let result = if unsafe { GetForegroundWindow() } != HWND(owner as *mut _) {
            Err("focus".to_owned())
        } else if generation.load(Ordering::Acquire) != expected {
            Err("cancelled".to_owned())
        } else {
            Geolocator::RequestAccessAsync().map_err(|_| "unavailable".to_owned())
        };
        let _ = send.send(result);
    }).map_err(|_| "unavailable")?;
    let generation = request.generation.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        // SAFETY: COM is initialized/balanced on this short-lived worker. The
        // permission operation was already started on the correct UI thread.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if initialized.is_err() { return Err("unavailable".into()); }
        let result = (|| {
            let permission = receive.recv_timeout(Duration::from_secs(3)).map_err(|_| "timeout")??;
            let access = wait(permission, &generation, expected, Duration::from_secs(60))?;
            if access != GeolocationAccessStatus::Allowed { return Err("denied".into()); }
            let locator = Geolocator::new().map_err(|_| "unavailable")?;
            locator.SetDesiredAccuracy(PositionAccuracy::Default).map_err(|_| "unavailable")?;
            let position = locator.GetGeopositionAsyncWithAgeAndTimeout(
                TimeSpan { Duration: 6 * 3600 * 10_000_000 },
                TimeSpan { Duration: 15 * 10_000_000 }).map_err(|_| "unavailable")?;
            let position = wait(position, &generation, expected, Duration::from_secs(17))?;
            let coordinate = position.Coordinate().map_err(|_| "unavailable")?;
            let point = coordinate.Point().and_then(|point| point.Position()).map_err(|_| "unavailable")?;
            let ticks = coordinate.Timestamp().map_err(|_| "unavailable")?.UniversalTime;
            let seconds = ticks.checked_sub(116_444_736_000_000_000).filter(|v| *v > 0).ok_or("unavailable")? / 10_000_000;
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|"unavailable")?.as_secs();
            if now.saturating_sub(seconds as u64)>6*3600 || seconds as u64>now+900 { return Err("unavailable".into()); }
            WeatherLocation::from_degrees(point.Latitude, point.Longitude, seconds as u64).ok_or("unavailable".into())
        })();
        unsafe { CoUninitialize(); }
        result
    }).await.map_err(|_| "unavailable".to_owned())?
}

#[tauri::command]
pub fn cancel_weather_location(request: State<'_, LocationRequest>) {
    request.generation.fetch_add(1, Ordering::AcqRel);
}

#[tauri::command]
pub fn open_weather_location_settings() -> Result<(), String> {
    muelle_shell::launcher::open("ms-settings:privacy-location").map_err(|_| "unavailable".into())
}

#[tauri::command]
pub fn open_weather_attribution() -> Result<(), String> {
    muelle_shell::launcher::open("https://open-meteo.com/").map_err(|_| "unavailable".into())
}
