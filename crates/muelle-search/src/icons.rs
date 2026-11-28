//! Loads shell icons on a background thread.

use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::HSTRING;

/// Icons are requested at this size and scaled down when drawn, which stays
/// sharp up to 200% display scaling.
const ICON_PIXELS: i32 = 64;

pub struct Icon {
    pub width: u32,
    pub height: u32,
    /// Top-down, premultiplied BGRA.
    pub pixels: Vec<u8>,
}

pub struct Loaded {
    pub target: String,
    pub icon: Option<Icon>,
}

/// Starts the loader. Send it targets; after each one it queues a `Loaded` and
/// posts `message` to `window`.
pub fn spawn(window: HWND, message: u32) -> (Sender<String>, Receiver<Loaded>) {
    let (requests, incoming) = channel::<String>();
    let (outgoing, results) = channel();
    let window = window.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        for target in incoming {
            let icon = unsafe { load(&target) };
            if outgoing.send(Loaded { target, icon }).is_err() {
                break;
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0));
            }
        }
    });
    (requests, results)
}

unsafe fn load(target: &str) -> Option<Icon> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(target), None).ok()?;
        let size = SIZE {
            cx: ICON_PIXELS,
            cy: ICON_PIXELS,
        };
        let bitmap = factory.GetImage(size, SIIGBF_ICONONLY).ok()?;
        let icon = read_pixels(bitmap);
        let _ = DeleteObject(bitmap.into());
        icon
    }
}

unsafe fn read_pixels(bitmap: HBITMAP) -> Option<Icon> {
    unsafe {
        let mut info = BITMAP::default();
        let size = size_of::<BITMAP>() as i32;
        if GetObjectW(bitmap.into(), size, Some(&mut info as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (width, height) = (info.bmWidth, info.bmHeight);
        if width <= 0 || height <= 0 || width > 1024 || height > 1024 {
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height asks for rows from the top down.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut header,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        // Old-style bitmaps carry no alpha channel at all.
        if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p[3] = 255);
        }
        Some(Icon {
            width: width as u32,
            height: height as u32,
            pixels,
        })
    }
}
