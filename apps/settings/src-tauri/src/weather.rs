//! Foreground, one-shot Windows location. The Dock never borrows Settings'
//! identity/permission: only an approximate fix crosses the config boundary.
use muelle_config::WeatherLocation;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::State;
use windows::{
    Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy},
    Foundation::TimeSpan,
    Win32::{
        Foundation::HWND,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
        UI::WindowsAndMessaging::GetForegroundWindow,
    },
};
use windows_future::{AsyncOperationCompletedHandler, AsyncStatus, IAsyncOperation};

#[derive(Default)]
struct Wake {
    gate: Mutex<()>,
    changed: Condvar,
}
impl Wake {
    fn notify(&self) {
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.changed.notify_all();
    }
}

#[derive(Default)]
pub struct LocationRequest {
    generation: Arc<AtomicU64>,
    busy: Arc<AtomicBool>,
    wake: Arc<Wake>,
}

struct Busy(Arc<AtomicBool>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn wait<T: windows_core::RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
    generation: &AtomicU64,
    expected: u64,
    timeout: Duration,
    wake: &Arc<Wake>,
) -> Result<T, String> {
    let until = Instant::now() + timeout;
    let completed = wake.clone();
    operation
        .SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
            completed.notify();
            Ok(())
        }))
        .map_err(|_| "unavailable")?;
    let mut guard = wake
        .gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if generation.load(Ordering::Acquire) != expected || Instant::now() >= until {
            drop(guard);
            let _ = operation.Cancel();
            return Err(if generation.load(Ordering::Acquire) != expected {
                "cancelled"
            } else {
                "timeout"
            }
            .into());
        }
        match operation.Status().map_err(|_| "unavailable")? {
            AsyncStatus::Started => {
                let (next, _) = wake
                    .changed
                    .wait_timeout(guard, until.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard = next;
            }
            _ => {
                drop(guard);
                return operation.GetResults().map_err(|error| {
                    if error.code() == windows::Win32::Foundation::E_ACCESSDENIED {
                        "denied"
                    } else {
                        "unavailable"
                    }
                    .to_owned()
                });
            }
        }
    }
}

#[tauri::command]
pub async fn locate_weather(
    window: tauri::WebviewWindow,
    request: State<'_, LocationRequest>,
) -> Result<WeatherLocation, String> {
    if request
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("busy".into());
    }
    let guard = Busy(request.busy.clone());
    let expected = request.generation.load(Ordering::Acquire);
    let generation = request.generation.clone();
    let owner = window.hwnd().map_err(|_| "unavailable")?.0 as isize;
    let (send, receive) = mpsc::sync_channel(1);
    // RequestAccessAsync must START on the foreground UI thread. The closure
    // returns immediately; no consent or position wait blocks that thread.
    window
        .run_on_main_thread(move || {
            let result = if unsafe { GetForegroundWindow() } != HWND(owner as *mut _) {
                Err("focus".to_owned())
            } else if generation.load(Ordering::Acquire) != expected {
                Err("cancelled".to_owned())
            } else {
                Geolocator::RequestAccessAsync().map_err(|_| "unavailable".to_owned())
            };
            if let Err(mpsc::SendError(Ok(operation))) = send.send(result) {
                // If the worker timed out before this UI closure ran, do not
                // leave an orphaned Windows consent operation behind.
                let _ = operation.Cancel();
            }
        })
        .map_err(|_| "unavailable")?;
    let generation = request.generation.clone();
    let wake = request.wake.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        // SAFETY: COM is initialized/balanced on this short-lived worker. The
        // permission operation was already started on the correct UI thread.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if initialized.is_err() {
            return Err("unavailable".into());
        }
        let result = (|| {
            let permission = match receive.recv_timeout(Duration::from_secs(3)) {
                Ok(permission) => permission?,
                Err(_) => {
                    let _ = generation.compare_exchange(
                        expected,
                        expected.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    wake.notify();
                    return Err("timeout".into());
                }
            };
            let access = wait(
                permission,
                &generation,
                expected,
                Duration::from_secs(60),
                &wake,
            )?;
            if access != GeolocationAccessStatus::Allowed {
                return Err("denied".into());
            }
            let locator = Geolocator::new().map_err(|_| "unavailable")?;
            locator
                .SetDesiredAccuracy(PositionAccuracy::Default)
                .map_err(|_| "unavailable")?;
            let position = locator
                .GetGeopositionAsyncWithAgeAndTimeout(
                    TimeSpan {
                        Duration: 6 * 3600 * 10_000_000,
                    },
                    TimeSpan {
                        Duration: 15 * 10_000_000,
                    },
                )
                .map_err(|_| "unavailable")?;
            let position = wait(
                position,
                &generation,
                expected,
                Duration::from_secs(17),
                &wake,
            )?;
            let coordinate = position.Coordinate().map_err(|_| "unavailable")?;
            let point = coordinate
                .Point()
                .and_then(|point| point.Position())
                .map_err(|_| "unavailable")?;
            let ticks = coordinate
                .Timestamp()
                .map_err(|_| "unavailable")?
                .UniversalTime;
            let seconds = ticks
                .checked_sub(116_444_736_000_000_000)
                .filter(|v| *v > 0)
                .ok_or("unavailable")?
                / 10_000_000;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "unavailable")?
                .as_secs();
            if now.saturating_sub(seconds as u64) > 6 * 3600 || seconds as u64 > now + 900 {
                return Err("unavailable".into());
            }
            WeatherLocation::from_degrees(point.Latitude, point.Longitude, seconds as u64)
                .ok_or("unavailable".into())
        })();
        unsafe {
            CoUninitialize();
        }
        result
    })
    .await
    .map_err(|_| "unavailable".to_owned())?
}

#[tauri::command]
pub fn cancel_weather_location(request: State<'_, LocationRequest>) {
    request.generation.fetch_add(1, Ordering::AcqRel);
    request.wake.notify();
}

#[tauri::command]
pub fn open_weather_location_settings() -> Result<(), String> {
    muelle_shell::launcher::open("ms-settings:privacy-location").map_err(|_| "unavailable".into())
}

#[tauri::command]
pub fn open_weather_attribution() -> Result<(), String> {
    muelle_shell::launcher::open("https://open-meteo.com/").map_err(|_| "unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_location_operations_wake_without_polling_or_prompting() {
        let operation = IAsyncOperation::ready(Ok(GeolocationAccessStatus::Allowed));
        let result = wait(
            operation,
            &AtomicU64::new(7),
            7,
            Duration::from_secs(1),
            &Arc::new(Wake::default()),
        );
        assert_eq!(result, Ok(GeolocationAccessStatus::Allowed));
    }

    #[test]
    fn location_wait_respects_a_deadline_and_reports_native_access_denial() {
        let (finish, receive) = mpsc::sync_channel(1);
        let operation = IAsyncOperation::spawn(move || {
            let _ = receive.recv_timeout(Duration::from_secs(5));
            Ok(GeolocationAccessStatus::Allowed)
        });
        let outcome = wait(
            operation,
            &AtomicU64::new(0),
            0,
            Duration::from_millis(10),
            &Arc::new(Wake::default()),
        );
        let _ = finish.send(());
        assert_eq!(outcome, Err("timeout".into()));
        let denied = IAsyncOperation::<GeolocationAccessStatus>::ready(Err(
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_ACCESSDENIED),
        ));
        assert_eq!(
            wait(
                denied,
                &AtomicU64::new(0),
                0,
                Duration::from_secs(1),
                &Arc::new(Wake::default())
            ),
            Err("denied".into())
        );
    }

    #[test]
    fn cancelling_a_wait_wakes_it_without_waiting_for_the_location_deadline() -> Result<(), String>
    {
        let (finish, receive) = mpsc::sync_channel(1);
        let operation = IAsyncOperation::spawn(move || {
            let _ = receive.recv_timeout(Duration::from_secs(5));
            Ok(GeolocationAccessStatus::Allowed)
        });
        let generation = Arc::new(AtomicU64::new(0));
        let wake = Arc::new(Wake::default());
        let expected_generation = generation.clone();
        let expected_wake = wake.clone();
        let (send_result, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let outcome = wait(
                operation,
                &expected_generation,
                0,
                Duration::from_secs(60),
                &expected_wake,
            );
            let _ = send_result.send(outcome);
        });
        generation.store(1, Ordering::Release);
        wake.notify();
        let outcome = result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "cancel did not wake the wait")?;
        assert_eq!(outcome, Err("cancelled".to_owned()));
        let _ = finish.send(());
        worker.join().map_err(|_| "wait worker panicked")?;
        Ok(())
    }
}
