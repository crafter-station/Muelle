//! The notifications waiting in the Windows notification center, for the
//! count on the menu bar's bell (issue #55).
//!
//! Read only while the bell is shown. Windows lists the notification center
//! to desktop apps through `UserNotificationListener` when notification
//! access is allowed (Settings → Privacy & security → Notifications); when
//! it is not, there is no count and nothing is read. Windows reports changes
//! with `NotificationChanged` where it supports that for desktop apps; the
//! menu bar also reads again with its minute tick, so a missed change is
//! caught up without polling of its own.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, Ordering},
    },
};

use windows::{
    Foundation::TypedEventHandler,
    UI::Notifications::{
        Management::{UserNotificationListener, UserNotificationListenerAccessStatus},
        NotificationKinds, UserNotification, UserNotificationChangedEventArgs,
    },
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::PostMessageW,
    },
};
use windows_collections::IVectorView;
use windows_core::Ref;
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

/// `WPARAM` of the window message: something changed, read again.
pub const CHANGED: usize = 0;
/// `WPARAM` of the window message: a read finished, see [`Watch::ids`].
pub const READ: usize = 1;

type Read = IAsyncOperation<IVectorView<UserNotification>>;

struct Shared {
    window: AtomicIsize,
    message: u32,
    /// The notifications' ids at the last read; `None` until one finishes.
    ids: Mutex<Option<Vec<u32>>>,
    /// The read in progress: one at a time.
    pending: Mutex<Option<Read>>,
}

impl Shared {
    fn post(&self, what: usize) {
        let window = self.window.load(Ordering::Acquire);
        if window != 0 {
            // SAFETY: Posts to the menu bar's own window; Windows ignores a
            // window that no longer exists.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(window as *mut _)),
                    self.message,
                    WPARAM(what),
                    LPARAM(0),
                );
            }
        }
    }
}

pub struct Watch {
    shared: Arc<Shared>,
    listener: UserNotificationListener,
    changed: Option<i64>,
}

impl Watch {
    /// Starts watching and reads once, or `None` when Windows does not let
    /// Muelle read the notification center.
    pub fn start(window: isize, message: u32) -> Option<Self> {
        let listener = UserNotificationListener::Current().ok()?;
        if listener.GetAccessStatus().ok()? != UserNotificationListenerAccessStatus::Allowed {
            return None;
        }
        let shared = Arc::new(Shared {
            window: AtomicIsize::new(window),
            message,
            ids: Mutex::new(None),
            pending: Mutex::new(None),
        });
        let weak = Arc::downgrade(&shared);
        // Not every Windows version raises this for desktop apps; the minute
        // tick reads again either way.
        let changed = listener
            .NotificationChanged(&TypedEventHandler::new(
                move |_: Ref<UserNotificationListener>,
                      _: Ref<UserNotificationChangedEventArgs>| {
                    if let Some(shared) = weak.upgrade() {
                        shared.post(CHANGED);
                    }
                    Ok(())
                },
            ))
            .ok();
        let watch = Self {
            shared,
            listener,
            changed,
        };
        watch.refresh();
        Some(watch)
    }

    /// Reads the notification center again, unless a read is under way.
    /// The window gets the message with [`READ`] when it finishes.
    pub fn refresh(&self) {
        let Ok(operation) = self
            .listener
            .GetNotificationsAsync(NotificationKinds::Toast)
        else {
            return;
        };
        // Claim the slot, then let go of the lock: the completion handler
        // takes it too, and may run at once if the read is already done.
        match self.shared.pending.lock() {
            Ok(mut pending) if pending.is_none() => *pending = Some(operation.clone()),
            _ => {
                let _ = operation.Cancel();
                return;
            }
        }
        let weak = Arc::downgrade(&self.shared);
        let handler = AsyncOperationCompletedHandler::new(move |operation: Ref<Read>, _| {
            if let Some(shared) = weak.upgrade() {
                if let Some(operation) = operation.as_ref()
                    && let Ok(list) = operation.GetResults()
                {
                    let ids = (0..list.Size().unwrap_or(0))
                        .filter_map(|index| list.GetAt(index).and_then(|item| item.Id()).ok())
                        .collect();
                    if let Ok(mut slot) = shared.ids.lock() {
                        *slot = Some(ids);
                    }
                    shared.post(READ);
                }
                if let Ok(mut pending) = shared.pending.lock() {
                    pending.take();
                }
            }
            Ok(())
        });
        if operation.SetCompleted(&handler).is_err()
            && let Ok(mut pending) = self.shared.pending.lock()
            && let Some(operation) = pending.take()
        {
            let _ = operation.Cancel();
        }
    }

    /// The notifications in the center at the last read finished, if any.
    pub fn ids(&self) -> Option<Vec<u32>> {
        self.shared.ids.lock().ok()?.clone()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.shared.window.store(0, Ordering::Release);
        if let Some(token) = self.changed.take() {
            let _ = self.listener.RemoveNotificationChanged(token);
        }
        if let Ok(mut pending) = self.shared.pending.lock()
            && let Some(operation) = pending.take()
        {
            let _ = operation.Cancel();
        }
    }
}

/// How many of `ids` were not there when the notification center was last
/// opened from the menu bar (`seen`).
#[must_use]
pub fn unread(ids: &[u32], seen: &HashSet<u32>) -> u32 {
    ids.iter().filter(|id| !seen.contains(id)).count() as u32
}

/// The badge's text: the count, or "99+" past that.
#[must_use]
pub fn badge_text(unread: u32) -> String {
    if unread > 99 {
        "99+".to_owned()
    } else {
        unread.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_notifications_newer_than_the_last_look_count() {
        let seen: HashSet<u32> = [1, 2, 3].into_iter().collect();
        assert_eq!(unread(&[1, 2, 3], &seen), 0);
        assert_eq!(unread(&[2, 3, 4, 5], &seen), 2);
        // Cleared in the notification center: nothing left to count.
        assert_eq!(unread(&[], &seen), 0);
    }

    #[test]
    fn the_badge_stays_short() {
        assert_eq!(badge_text(1), "1");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
    }

    #[test]
    #[ignore = "Reads the real notification center; changes nothing"]
    fn native_listener_reads_the_notification_center_when_allowed() {
        // Without notification access there is simply no watch.
        let Some(watch) = Watch::start(0, 0) else {
            return;
        };
        for _ in 0..50 {
            if watch.ids().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(watch.ids().is_some(), "the first read should finish");
    }
}
