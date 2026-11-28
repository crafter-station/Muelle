//! Promoted Windows notification icons. No taskbar reveal, injection or polling.
//! Windows remains the visibility authority. UIA and registry access run off UI.
#[cfg(test)]
#[path = "tray/fixture.rs"]
mod fixture;
#[path = "tray/native.rs"]
mod native;
#[path = "tray/registry.rs"]
mod registry;
pub fn registry_path() -> windows::core::PCWSTR {
    registry::PATH
}

use muelle_shell::icons::IconImage;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    UI::WindowsAndMessaging::PostMessageW,
};

#[derive(Clone)]
pub struct Icon {
    pub id: u64,
    pub name: String,
    pub image: Arc<IconImage>,
    png: Arc<[u8]>,
}
#[derive(Clone, Default)]
pub struct Snapshot(Arc<Vec<Icon>>);
impl Snapshot {
    pub fn icons(&self) -> &[Icon] {
        &self.0
    }
}
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || (self.0.is_empty() && other.0.is_empty())
    }
}

enum Command {
    Refresh,
    Invoke(u64, bool),
}
#[derive(Clone)]
struct Signal {
    sender: SyncSender<Command>,
    pending: Arc<AtomicBool>,
}
impl Signal {
    fn refresh(&self) {
        if !self.pending.swap(true, Ordering::AcqRel)
            && self.sender.try_send(Command::Refresh).is_err()
        {
            self.pending.store(false, Ordering::Release);
        }
    }
}
pub struct Watch {
    signal: Signal,
    snapshot: Arc<Mutex<Snapshot>>,
    target: Arc<AtomicIsize>,
    stopped: Arc<AtomicBool>,
}
impl Watch {
    pub fn new(hwnd: isize, message: u32) -> Option<Self> {
        let (sender, receiver) = mpsc::sync_channel(16);
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let target = Arc::new(AtomicIsize::new(hwnd));
        let stopped = Arc::new(AtomicBool::new(false));
        let output = snapshot.clone();
        let window = target.clone();
        let stop = stopped.clone();
        let signal = Signal {
            sender,
            pending: Arc::new(AtomicBool::new(false)),
        };
        let events = signal.clone();
        let pending = signal.pending.clone();
        std::thread::Builder::new()
            .name("tray-icons".into())
            .spawn(move || {
                // SAFETY: The worker owns the MTA and all UIA interfaces, destroyed
                // before CoUninitialize; no UI thread waits for a provider.
                if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.is_err() {
                    return;
                }
                if let Ok(mut client) = native::Client::new(events) {
                    let mut last_scan = Instant::now() - Duration::from_secs(1);
                    let mut retry = None;
                    while !stop.load(Ordering::Acquire) {
                        let mut command = match retry {
                            Some(delay) => match receiver.recv_timeout(delay) {
                                Ok(command) => command,
                                Err(RecvTimeoutError::Timeout) => Command::Refresh,
                                Err(RecvTimeoutError::Disconnected) => break,
                            },
                            None => match receiver.recv() {
                                Ok(command) => command,
                                Err(_) => break,
                            },
                        };
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        // Coalesce provider bursts without delaying user actions.
                        if matches!(command, Command::Refresh) {
                            let wait =
                                Duration::from_millis(250).saturating_sub(last_scan.elapsed());
                            let until = Instant::now() + wait;
                            while Instant::now() < until {
                                match receiver
                                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                                {
                                    Ok(next @ Command::Invoke(_, _)) => {
                                        command = next;
                                        break;
                                    }
                                    Ok(Command::Refresh) => {}
                                    Err(_) => break,
                                }
                            }
                            pending.store(false, Ordering::Release);
                        }
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let records = registry::records();
                        let owners = registry::owners(&records);
                        let buttons = if records.is_empty() {
                            Vec::new()
                        } else {
                            client.buttons().unwrap_or_default()
                        };
                        // Retry only a failed attachment (startup/Explorer restart).
                        // An attached, idle tray blocks on events without polling.
                        retry = if !records.is_empty() && !client.connected() {
                            Some(retry.map_or(Duration::from_secs(1), |d: Duration| {
                                (d * 2).min(Duration::from_secs(30))
                            }))
                        } else {
                            None
                        };
                        let old = output.lock().map(|s| s.clone()).unwrap_or_default();
                        let locations: Vec<_> = records
                            .iter()
                            .filter_map(|r| registry::location(r, &owners).map(|rect| (r.id, rect)))
                            .collect();
                        let mut icons = Vec::new();
                        for record in records {
                            let Some((_, rect)) = locations.iter().find(|(id, _)| *id == record.id)
                            else {
                                continue;
                            };
                            let mut matches =
                                buttons.iter().filter(|b| native::contains(b.rect, *rect));
                            let Some(button) = matches.next() else {
                                continue;
                            };
                            if matches.next().is_some() {
                                continue;
                            }
                            // Never paint two cached records for one live button or
                            // guess which app to activate when identities disagree.
                            if locations
                                .iter()
                                .filter(|(_, r)| native::contains(button.rect, *r))
                                .count()
                                != 1
                            {
                                continue;
                            }
                            if let Command::Invoke(id, context) = command
                                && id == record.id
                                && let Err(error) = native::Client::invoke(button, context)
                            {
                                muelle_shell::log::line(&format!(
                                    "bandeja: acción no disponible ({:?})",
                                    error.code()
                                ));
                            }
                            let Some(png) = registry::snapshot(record.id) else {
                                continue;
                            };
                            let previous = old
                                .icons()
                                .iter()
                                .find(|i| i.id == record.id && i.png.as_ref() == png);
                            let image = previous
                                .map(|i| i.image.clone())
                                .or_else(|| decode(&png).map(Arc::new));
                            let Some(image) = image else {
                                continue;
                            };
                            icons.push((
                                rect.left,
                                Icon {
                                    id: record.id,
                                    name: button.name.clone(),
                                    image,
                                    png: previous.map_or_else(|| Arc::from(png), |i| i.png.clone()),
                                },
                            ));
                        }
                        icons.sort_by_key(|(left, _)| *left);
                        let icons: Vec<_> = icons.into_iter().map(|(_, i)| i).collect();
                        let changed = icons.len() != old.icons().len()
                            || icons.iter().zip(old.icons()).any(|(a, b)| {
                                a.id != b.id || a.name != b.name || !Arc::ptr_eq(&a.image, &b.image)
                            });
                        if changed {
                            if let Ok(mut state) = output.lock() {
                                *state = Snapshot(Arc::new(icons));
                            }
                            let hwnd = window.load(Ordering::Acquire);
                            if hwnd != 0 {
                                // SAFETY: Target is cleared before Watch drops. Posting
                                // only; never wait on the UI thread from a provider.
                                unsafe {
                                    let _ = PostMessageW(
                                        Some(HWND(hwnd as _)),
                                        message,
                                        WPARAM(0),
                                        LPARAM(0),
                                    );
                                }
                            }
                        }
                        last_scan = Instant::now();
                        // Refresh signals are interchangeable; leave action commands
                        // ordered rather than draining and losing a click.
                    }
                }
                unsafe {
                    CoUninitialize();
                }
            })
            .ok()?;
        let watch = Self {
            signal,
            snapshot,
            target,
            stopped,
        };
        watch.refresh();
        Some(watch)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }
    pub fn refresh(&self) {
        self.signal.refresh();
    }
    pub fn invoke(&self, id: u64, context: bool) {
        let _ = self.signal.sender.try_send(Command::Invoke(id, context));
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.target.store(0, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
        let _ = self.signal.sender.try_send(Command::Refresh);
        // No join on UI: bounded UIA transactions and COM teardown stay on worker.
    }
}

fn decode(bytes: &[u8]) -> Option<IconImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: 1024 * 1024 });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    if reader.info().width > 256 || reader.info().height > 256 {
        return None;
    }
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let channels = match frame.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        _ => return None,
    };
    let mut bgra = Vec::with_capacity(frame.width as usize * frame.height as usize * 4);
    for pixel in pixels[..frame.buffer_size()].chunks_exact(channels) {
        let alpha = match channels {
            4 => pixel[3],
            2 => pixel[1],
            _ => 255,
        };
        let rgb = if channels <= 2 {
            [pixel[0]; 3]
        } else {
            [pixel[2], pixel[1], pixel[0]]
        };
        for c in rgb {
            bgra.push((u16::from(c) * u16::from(alpha) / 255) as u8);
        }
        bgra.push(alpha);
    }
    Some(IconImage {
        width: frame.width,
        height: frame.height,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshots_do_not_force_menu_bar_redraws() {
        assert!(Snapshot::default() == Snapshot::default());
    }

    #[test]
    fn rejects_invalid_icon_image() {
        assert!(decode(b"not a PNG icon").is_none());
    }

    #[test]
    fn grayscale_icons_keep_their_opacity_in_premultiplied_bgra() -> anyhow::Result<()> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::GrayscaleAlpha);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&[200, 128])?;
        let icon = decode(&bytes).ok_or_else(|| anyhow::anyhow!("PNG decode failed"))?;
        assert_eq!(icon.bgra, [100, 100, 100, 128]);
        Ok(())
    }

    #[test]
    #[ignore = "read-only probe against the signed-in Windows desktop"]
    fn live_hidden_taskbar_icons() -> anyhow::Result<()> {
        let watch = Watch::new(0, 0).ok_or_else(|| anyhow::anyhow!("worker could not start"))?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            let snapshot = watch.snapshot();
            if !snapshot.icons().is_empty() {
                println!(
                    "{} live promoted icons; decoded image sizes: {:?}",
                    snapshot.icons().len(),
                    snapshot
                        .icons()
                        .iter()
                        .map(|i| (i.image.width, i.image.height))
                        .collect::<Vec<_>>()
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("no promoted icon bound to a live accessible button")
    }
}
