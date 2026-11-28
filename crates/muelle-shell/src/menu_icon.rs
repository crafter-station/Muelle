//! The user's own picture for the menu bar's icon: what is accepted, the copy
//! Muelle keeps, and its pixels. Settings states these rules; both it and the
//! dock apply them here.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

use crate::{icons::IconImage, imagery};

/// The largest file accepted.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// The sides accepted, in pixels.
pub const MIN_SIDE: u32 = 32;
pub const MAX_SIDE: u32 = 1024;

/// Why a picture cannot be the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Not a PNG, ICO, JPG or BMP picture, or one that cannot be read.
    Format,
    /// The file is larger than `MAX_BYTES`.
    TooLarge,
    NotSquare,
    /// A side outside `MIN_SIDE` to `MAX_SIDE`.
    Dimensions,
    /// The copy could not be written.
    Save,
}

impl Problem {
    /// The name Settings knows it by.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::TooLarge => "size",
            Self::NotSquare => "square",
            Self::Dimensions => "dimensions",
            Self::Save => "save",
        }
    }
}

/// The extension for a file starting with these bytes; the name says nothing.
fn kind(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0, 0, 1, 0, ..] => Some("ico"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'B', b'M', ..] => Some("bmp"),
        _ => None,
    }
}

/// The file's bytes, kind and picture at `side` pixels, if it is acceptable.
fn read(path: &Path, side: u32) -> Result<(Vec<u8>, &'static str, IconImage), Problem> {
    let length = fs::metadata(path).map_err(|_| Problem::Format)?.len();
    if length > MAX_BYTES {
        return Err(Problem::TooLarge);
    }
    let bytes = fs::read(path).map_err(|_| Problem::Format)?;
    let extension = kind(&bytes).ok_or(Problem::Format)?;
    // SAFETY: Balanced below, once every COM object is released.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let picture = picture(&bytes, side);
    if com.is_ok() {
        // SAFETY: Balances the successful initialization above.
        unsafe { CoUninitialize() };
    }
    Ok((bytes, extension, picture?))
}

fn picture(bytes: &[u8], side: u32) -> Result<IconImage, Problem> {
    // An .ico holds several sizes: the largest is the one judged and used.
    let (frame, width, height) = imagery::largest_frame(bytes).ok_or(Problem::Format)?;
    if width != height {
        return Err(Problem::NotSquare);
    }
    if !(MIN_SIDE..=MAX_SIDE).contains(&width) {
        return Err(Problem::Dimensions);
    }
    imagery::decode_frame(bytes, frame, side, u64::from(MAX_SIDE * MAX_SIDE)).ok_or(Problem::Format)
}

/// The picture in `path` at `side` pixels.
pub fn load(path: &Path, side: u32) -> Result<IconImage, Problem> {
    read(path, side).map(|(_, _, image)| image)
}

fn folder() -> PathBuf {
    PathBuf::from(env::var_os("APPDATA").unwrap_or_default())
        .join("Muelle")
        .join("menu-icons")
}

/// Checks `source` and keeps a copy, so that moving or deleting the original
/// changes nothing. Returns the copy's path. Older copies go, except
/// `current`, which the settings still name until the new one is saved.
pub fn import(source: &Path, current: Option<&Path>) -> Result<PathBuf, Problem> {
    import_into(&folder(), source, current)
}

fn import_into(folder: &Path, source: &Path, current: Option<&Path>) -> Result<PathBuf, Problem> {
    let (bytes, extension, _) = read(source, MIN_SIDE)?;
    // Named by content (FNV-1a), so another picture is always another path,
    // which is how the dock notices it.
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let copy = folder.join(format!("icon-{hash:016x}.{extension}"));
    fs::create_dir_all(folder).map_err(|_| Problem::Save)?;
    fs::write(&copy, bytes).map_err(|_| Problem::Save)?;
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        let ours = entry.file_name().to_string_lossy().starts_with("icon-");
        if ours && path != copy && Some(path.as_path()) != current {
            // A copy left behind costs only its space.
            let _ = fs::remove_file(path);
        }
    }
    Ok(copy)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// An uncompressed 32-bit bitmap, all one colour.
    fn bitmap(width: u32, height: u32, shade: u8) -> Vec<u8> {
        let pixels = width * height * 4;
        let mut bytes = b"BM".to_vec();
        for value in [54 + pixels, 0, 54, 40, width, height] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        for value in [0, pixels, 0, 0, 0, 0] {
            bytes.extend(u32::to_le_bytes(value));
        }
        bytes.extend(std::iter::repeat_n(shade, pixels as usize));
        bytes
    }

    fn scratch(name: &str) -> PathBuf {
        let folder = env::temp_dir().join(format!("muelle-icon-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn only_square_pictures_of_a_sensible_size_are_accepted() {
        let folder = scratch("rules");
        let file = folder.join("picked");
        let judged = |bytes: Vec<u8>| {
            fs::write(&file, bytes).unwrap();
            load(&file, 16).map(|image| (image.width, image.height))
        };
        assert_eq!(judged(bitmap(64, 64, 200)), Ok((16, 16)));
        assert_eq!(judged(bitmap(64, 48, 200)), Err(Problem::NotSquare));
        assert_eq!(judged(bitmap(16, 16, 200)), Err(Problem::Dimensions));
        assert_eq!(judged(bitmap(1100, 1100, 0)), Err(Problem::TooLarge));
        assert_eq!(judged(b"<svg/>".to_vec()), Err(Problem::Format));
        assert_eq!(
            judged(b"BM but nothing more".to_vec()),
            Err(Problem::Format)
        );
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_kept_copy_outlives_the_original_and_replaces_older_ones() {
        let root = scratch("copies");
        let (kept, source) = (root.join("kept"), root.join("source.png"));
        // The content decides the extension, not the name.
        let pick = |shade, current: Option<&Path>| {
            fs::write(&source, bitmap(32, 32, shade)).unwrap();
            import_into(&kept, &source, current).unwrap()
        };
        let first = pick(1, None);
        assert_eq!(first.extension().unwrap(), "bmp");
        let second = pick(2, Some(&first));
        assert!(first != second && first.exists() && second.exists());
        let third = pick(3, Some(&second));
        assert!(!first.exists() && second.exists() && third.exists());
        fs::remove_file(&source).unwrap();
        assert!(load(&third, 32).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
