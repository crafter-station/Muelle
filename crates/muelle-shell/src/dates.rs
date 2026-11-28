//! Today's date and the words Windows has for months and weekdays, for the
//! menu bar's calendar.

use windows::{
    Win32::{
        Foundation::SYSTEMTIME,
        Globalization::{
            DATE_YEARMONTH, GetDateFormatEx, GetLocaleInfoEx, GetUserDefaultLocaleName,
            LOCALE_IFIRSTDAYOFWEEK, LOCALE_SSHORTESTDAYNAME1,
        },
        System::SystemInformation::GetLocalTime,
    },
    core::{HSTRING, PCWSTR},
};

/// Today, in local time: year, month from 1, day from 1.
#[must_use]
pub fn today() -> (i32, u32, u32) {
    // SAFETY: Read-only query of the clock.
    let now = unsafe { GetLocalTime() };
    (
        i32::from(now.wYear),
        u32::from(now.wMonth),
        u32::from(now.wDay),
    )
}

/// A buffer Windows filled with `length` units, the last of them the
/// terminator; `None` if it failed.
fn filled(buffer: &[u16], length: i32) -> Option<String> {
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    Some(String::from_utf16_lossy(buffer.get(..length)?))
}

/// The user's own locale if it is of `language` (such as `es`), so that their
/// regional wording shows; otherwise `fallback`.
#[must_use]
pub fn locale_for(language: &str, fallback: &str) -> String {
    let mut buffer = [0u16; 85];
    // SAFETY: The buffer lives for the call.
    let length = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    filled(&buffer, length)
        .filter(|user| user.split('-').next() == Some(language))
        .unwrap_or_else(|| fallback.to_owned())
}

/// The day the user's week starts on, from Windows' regional settings: 0 for
/// Monday to 6 for Sunday.
#[must_use]
pub fn first_weekday() -> u8 {
    let mut buffer = [0u16; 8];
    // SAFETY: The user's own locale and a buffer that lives for the call.
    let length =
        unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_IFIRSTDAYOFWEEK, Some(&mut buffer)) };
    filled(&buffer, length)
        .and_then(|day| day.trim().parse().ok())
        .filter(|&day| day < 7)
        .unwrap_or(0)
}

/// A month's name with its year, as `locale` writes them.
#[must_use]
pub fn month_title(locale: &str, year: i32, month: u32) -> Option<String> {
    let date = SYSTEMTIME {
        wYear: u16::try_from(year).ok()?,
        wMonth: u16::try_from(month).ok()?,
        wDay: 1,
        ..Default::default()
    };
    let mut buffer = [0u16; 80];
    // SAFETY: The strings, date and buffer live for the call.
    let length = unsafe {
        GetDateFormatEx(
            &HSTRING::from(locale),
            DATE_YEARMONTH,
            Some(&date),
            PCWSTR::null(),
            Some(&mut buffer),
            PCWSTR::null(),
        )
    };
    filled(&buffer, length)
}

/// The shortest name `locale` has for a weekday, 0 being Monday.
#[must_use]
pub fn shortest_day_name(locale: &str, weekday: u8) -> Option<String> {
    let mut buffer = [0u16; 32];
    // SAFETY: The name and buffer live for the call. The seven names have
    // consecutive numbers, Monday first.
    let length = unsafe {
        GetLocaleInfoEx(
            &HSTRING::from(locale),
            LOCALE_SSHORTESTDAYNAME1 + u32::from(weekday.min(6)),
            Some(&mut buffer),
        )
    };
    filled(&buffer, length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_names_months_and_weekdays_in_the_locale_asked() {
        let title = month_title("en-US", 2023, 8).unwrap_or_default();
        assert!(
            title.contains("August") && title.contains("2023"),
            "{title}"
        );
        assert!(shortest_day_name("en-US", 0).is_some_and(|name| name.starts_with('M')));
        assert!(shortest_day_name("en-US", 6).is_some_and(|name| name.starts_with('S')));
        assert!(first_weekday() < 7);
        // A language that is not the user's takes the fallback.
        assert_eq!(locale_for("xx", "en-US"), "en-US");
    }
}
