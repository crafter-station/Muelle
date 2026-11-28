//! UI Automation stays on its owning MTA thread, including actions and teardown.
use super::Signal;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY},
        UI::{
            Accessibility::*,
            WindowsAndMessaging::{EnumChildWindows, FindWindowW, GetClassNameW},
        },
    },
    core::{BOOL, Interface, Ref, implement, w},
};

#[implement(IUIAutomationStructureChangedEventHandler)]
struct Changed(Signal);
impl IUIAutomationStructureChangedEventHandler_Impl for Changed_Impl {
    fn HandleStructureChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: StructureChangeType,
        _: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        self.0.refresh();
        Ok(())
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct PropertyChanged(Signal);
impl IUIAutomationPropertyChangedEventHandler_Impl for PropertyChanged_Impl {
    fn HandlePropertyChangedEvent(
        &self,
        sender: Ref<IUIAutomationElement>,
        _: UIA_PROPERTY_ID,
        _: &windows::Win32::System::Variant::VARIANT,
    ) -> windows::core::Result<()> {
        // Cached event data only: callbacks never call back into Explorer.
        if sender.as_ref().is_some_and(|element| unsafe {
            element
                .CachedAutomationId()
                .is_ok_and(|id| id == "NotifyItemIcon")
        }) {
            self.0.refresh();
        }
        Ok(())
    }
}

pub(super) struct Client {
    automation: IUIAutomation2,
    root: Option<IUIAutomationElement>,
    event_root: Option<IUIAutomationElement>,
    handler: IUIAutomationStructureChangedEventHandler,
    property_handler: IUIAutomationPropertyChangedEventHandler,
}
pub(super) struct Button {
    pub element: IUIAutomationElement,
    pub rect: RECT,
    pub name: String,
}

impl Client {
    pub fn new(sender: Signal) -> windows::core::Result<Self> {
        // SAFETY: Caller owns an initialized MTA; all objects remain on it.
        unsafe {
            let automation: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            automation.SetConnectionTimeout(800)?;
            automation.SetTransactionTimeout(800)?;
            automation.SetAutoSetFocus(false)?;
            Ok(Self {
                automation,
                root: None,
                event_root: None,
                handler: Changed(sender.clone()).into(),
                property_handler: PropertyChanged(sender).into(),
            })
        }
    }

    fn connect(&mut self) -> windows::core::Result<IUIAutomationElement> {
        if let Some(root) = &self.root {
            return Ok(root.clone());
        }
        unsafe extern "system" fn child(window: HWND, data: LPARAM) -> BOOL {
            let result = unsafe { &mut *(data.0 as *mut Vec<isize>) };
            let mut class = [0u16; 128];
            // SAFETY: Fixed capacity class-name buffer; EnumChildWindows supplies HWND.
            let len = unsafe { GetClassNameW(window, &mut class) } as usize;
            if matches!(
                String::from_utf16_lossy(&class[..len]).as_str(),
                "Windows.UI.Composition.DesktopWindowContentBridge"
                    | "Windows.UI.Input.InputSite.WindowClass"
            ) {
                result.push(window.0 as isize);
            }
            (result.len() < 8).into()
        }
        // SAFETY: Read-only traversal. The bridge exposes the real tray buttons even
        // when Shell_TrayWnd itself exposes only its legacy, empty accessibility tree.
        unsafe {
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), None)?;
            let mut bridges = Vec::<isize>::new();
            let _ = EnumChildWindows(
                Some(taskbar),
                Some(child),
                LPARAM((&raw mut bridges) as isize),
            );
            if bridges.is_empty() {
                bridges.push(taskbar.0 as isize);
            }
            let condition = self.automation.CreateOrCondition(
                &self.automation.CreatePropertyCondition(
                    UIA_AutomationIdPropertyId,
                    &"SystemTrayIcon".into(),
                )?,
                &self.automation.CreatePropertyCondition(
                    UIA_AutomationIdPropertyId,
                    &"NotifyItemIcon".into(),
                )?,
            )?;
            // Shell builds can have several XAML bridges. Attach to the tray's
            // actual provider rather than keeping an empty search/task-list root.
            let root = bridges
                .into_iter()
                .find_map(|handle| {
                    let root = self.automation.ElementFromHandle(HWND(handle as _)).ok()?;
                    root.FindFirst(TreeScope_Descendants, &condition).ok()?;
                    Some(root)
                })
                .ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
                })?;
            let cache = self.automation.CreateCacheRequest()?;
            cache.AddProperty(UIA_AutomationIdPropertyId)?;
            // The XAML bridge is a query fragment, not the event tree's owner.
            // Subscribe through Shell's HWND root so removal events are in scope.
            let event_root = self.automation.ElementFromHandle(taskbar)?;
            self.automation.AddStructureChangedEventHandler(
                &event_root,
                TreeScope_Subtree,
                None,
                &self.handler,
            )?;
            self.event_root = Some(event_root.clone());
            let _ = self.automation.AddPropertyChangedEventHandlerNativeArray(
                &event_root,
                TreeScope_Descendants,
                &cache,
                &self.property_handler,
                &[UIA_NamePropertyId, UIA_IsEnabledPropertyId],
            );
            self.root = Some(root.clone());
            Ok(root)
        }
    }

    pub fn buttons(&mut self) -> windows::core::Result<Vec<Button>> {
        let root = self.connect()?;
        // SAFETY: Batched property cache avoids one cross-process call per property.
        let result = (|| unsafe {
            let cache = self.automation.CreateCacheRequest()?;
            for property in [
                UIA_AutomationIdPropertyId,
                UIA_NamePropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_IsEnabledPropertyId,
            ] {
                cache.AddProperty(property)?;
            }
            let condition = self
                .automation
                .CreatePropertyCondition(UIA_AutomationIdPropertyId, &"NotifyItemIcon".into())?;
            let nodes = root.FindAllBuildCache(TreeScope_Descendants, &condition, &cache)?;
            let mut buttons = Vec::new();
            for index in 0..nodes.Length()?.min(32) {
                let element = nodes.GetElement(index)?;
                if !element.CachedIsEnabled()?.as_bool() {
                    continue;
                }
                // An image alone is not an actionable notification icon.
                if element
                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                    .is_err()
                {
                    continue;
                }
                buttons.push(Button {
                    rect: element.CachedBoundingRectangle()?,
                    name: element.CachedName()?.to_string(),
                    element,
                });
            }
            Ok(buttons)
        })();
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    pub fn connected(&self) -> bool {
        self.root.is_some()
    }

    pub fn invoke(button: &Button, context: bool) -> windows::core::Result<()> {
        // SAFETY: Only the real, currently bound button is invoked. No fabricated
        // callback message, keyboard focus changes or Explorer memory access.
        unsafe {
            if context {
                button
                    .element
                    .cast::<IUIAutomationElement3>()?
                    .ShowContextMenu()
            } else {
                button
                    .element
                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                    .Invoke()
            }
        }
    }

    fn disconnect(&mut self) {
        // SAFETY: Registrations and COM objects belong to this MTA.
        unsafe {
            if let Some(root) = self.event_root.take() {
                let _ = self
                    .automation
                    .RemoveStructureChangedEventHandler(&root, &self.handler);
                let _ = self
                    .automation
                    .RemovePropertyChangedEventHandler(&root, &self.property_handler);
            }
        }
        self.root = None;
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Native icon bounds must fall inside exactly one current tray button.
pub(super) fn contains(button: RECT, icon: RECT) -> bool {
    icon.right > icon.left
        && icon.bottom > icon.top
        && button.left <= icon.left
        && button.top <= icon.top
        && button.right >= icon.right
        && button.bottom >= icon.bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_zero_bounds_never_match_a_live_button() {
        let button = RECT {
            left: -40,
            top: -20,
            right: 20,
            bottom: 20,
        };
        assert!(!contains(button, RECT::default()));
    }
    #[test]
    fn nearby_icon_cannot_activate_another_button() {
        let button = RECT {
            left: 100,
            top: 800,
            right: 140,
            bottom: 840,
        };
        let neighbor = RECT {
            left: 140,
            top: 800,
            right: 180,
            bottom: 840,
        };
        assert!(!contains(button, neighbor));
    }
    #[test]
    fn supports_negative_monitor_coordinates_and_scaled_bounds() {
        let button = RECT {
            left: -120,
            top: -60,
            right: -80,
            bottom: 0,
        };
        let icon = RECT {
            left: -110,
            top: -40,
            right: -90,
            bottom: -20,
        };
        assert!(contains(button, icon));
    }
}
