//! Built-in display brightness through WMI (`ROOT\WMI`).
//!
//! Works on laptop panels only; desktops and external monitors have no
//! `WmiMonitorBrightness` instance. Both functions block for tens of
//! milliseconds (more on the first call): call them from a worker thread.

use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
            },
            Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
            Variant::VARIANT,
            Wmi::{
                IEnumWbemClassObject, IWbemClassObject, IWbemContext, IWbemLocator, IWbemServices,
                WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
                WbemLocator,
            },
        },
    },
    core::{BSTR, IUnknown, Interface, PCWSTR, w},
};

#[derive(Debug, thiserror::Error)]
pub enum BrightnessError {
    #[error("Esta pantalla no permite cambiar el brillo")]
    Unsupported,
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

/// Initializes COM for the calling thread when needed and balances it on drop.
struct ComGuard(bool);

impl ComGuard {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: Plain COM initialization; undone in `Drop` only when this
        // call succeeded (S_OK or S_FALSE).
        let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if status.is_ok() {
            Ok(Self(true))
        } else if status == RPC_E_CHANGED_MODE {
            // The thread already runs an apartment: usable as it is.
            Ok(Self(false))
        } else {
            Err(windows::core::Error::from_hresult(status))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: Balances the successful `CoInitializeEx` in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Sets the per-proxy security WMI needs without touching process security.
fn blanket<T: Interface>(proxy: &T) -> windows::core::Result<()> {
    let unknown: IUnknown = proxy.cast()?;
    // SAFETY: `unknown` is a live COM interface for the call.
    unsafe {
        CoSetProxyBlanket(
            &unknown,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    }
}

fn connect() -> windows::core::Result<IWbemServices> {
    // SAFETY: COM is initialized by the caller's `ComGuard`; all arguments
    // are owned values that outlive the calls.
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let services = locator.ConnectServer(
            &BSTR::from("ROOT\\WMI"),
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None::<&IWbemContext>,
        )?;
        blanket(&services)?;
        Ok(services)
    }
}

/// The first instance of `class`, if any.
fn first_instance(
    services: &IWbemServices,
    class: &str,
) -> windows::core::Result<Option<IWbemClassObject>> {
    // SAFETY: Strings and the out slice live for the calls; `returned` tells
    // how many slots `Next` filled.
    unsafe {
        let rows: IEnumWbemClassObject = services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(format!("SELECT * FROM {class}")),
            WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0),
            None::<&IWbemContext>,
        )?;
        blanket(&rows)?;
        let mut slot = [None];
        let mut returned = 0u32;
        rows.Next(-1, &mut slot, &raw mut returned).ok()?;
        Ok(if returned == 0 { None } else { slot[0].take() })
    }
}

fn read(services: &IWbemServices) -> windows::core::Result<Option<u8>> {
    let Some(instance) = first_instance(services, "WmiMonitorBrightness")? else {
        return Ok(None);
    };
    let mut value = VARIANT::default();
    // SAFETY: `value` outlives the call, which fills it.
    unsafe { instance.Get(w!("CurrentBrightness"), 0, &raw mut value, None, None)? };
    let percent = u32::try_from(&value)?;
    Ok(Some(percent.min(100) as u8))
}

/// The built-in display brightness in percent; `None` without a controllable
/// panel (desktops, external monitors) or when WMI fails. Blocks: call it
/// from a worker thread.
#[must_use]
pub fn get() -> Option<u8> {
    let _com = ComGuard::new().ok()?;
    read(&connect().ok()?).ok()?
}

/// What a write needs, looked up once: the connection, the method path of
/// the panel and the method's parameter signature.
struct Context {
    services: IWbemServices,
    path: BSTR,
    signature: IWbemClassObject,
}

impl Context {
    fn open() -> Result<Self, BrightnessError> {
        let services = connect()?;
        let target = first_instance(&services, "WmiMonitorBrightnessMethods")?
            .ok_or(BrightnessError::Unsupported)?;
        // SAFETY: Every pointer and string lives for its call.
        unsafe {
            let mut path = VARIANT::default();
            target.Get(w!("__PATH"), 0, &raw mut path, None, None)?;
            let path = BSTR::try_from(&path)?;

            let mut class = None;
            services.GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                Some(&raw mut class),
                None,
            )?;
            let class = class.ok_or(BrightnessError::Unsupported)?;
            let mut signature = None;
            class.GetMethod(
                w!("WmiSetBrightness"),
                0,
                &raw mut signature,
                std::ptr::null_mut(),
            )?;
            Ok(Self {
                services,
                path,
                signature: signature.ok_or(BrightnessError::Unsupported)?,
            })
        }
    }

    fn write(&self, percent: u8) -> Result<(), BrightnessError> {
        // SAFETY: Every pointer and string lives for its call; the parameters
        // object comes from the method signature of the same class.
        unsafe {
            let params = self.signature.SpawnInstance(0)?;
            params.Put(w!("Timeout"), 0, &VARIANT::from(1u32), 0)?;
            params.Put(w!("Brightness"), 0, &VARIANT::from(percent.min(100)), 0)?;
            self.services.ExecMethod(
                &self.path,
                &BSTR::from("WmiSetBrightness"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                &params,
                None,
                None,
            )?;
        }
        Ok(())
    }
}

/// Writes the built-in display brightness, keeping the WMI connection between
/// writes so a slider drag does not reconnect for every step. Lives on one
/// worker thread, which it initializes COM for.
#[derive(Default)]
pub struct Writer {
    // Declared before `com`: the interfaces must go before COM is undone.
    context: Option<Context>,
    com: Option<ComGuard>,
}

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the brightness (clamped to 0 to 100). A failure on a kept
    /// connection reconnects once before giving up. Blocks: call it from the
    /// worker thread that owns this writer.
    ///
    /// # Errors
    /// [`BrightnessError::Unsupported`] without a controllable panel, or the
    /// Windows error.
    pub fn set(&mut self, percent: u8) -> Result<(), BrightnessError> {
        if self.com.is_none() {
            self.com = Some(ComGuard::new()?);
        }
        if let Some(context) = &self.context {
            if context.write(percent).is_ok() {
                return Ok(());
            }
            // Stale connection (WMI restarted, display changed): start over.
            self.context = None;
        }
        let context = Context::open()?;
        let result = context.write(percent);
        if result.is_ok() {
            self.context = Some(context);
        }
        result
    }
}

/// Sets the built-in display brightness (clamped to 0 to 100) with a
/// one-off connection. Blocks: call it from a worker thread.
///
/// # Errors
/// [`BrightnessError::Unsupported`] without a controllable panel, or the
/// Windows error.
pub fn set(percent: u8) -> Result<(), BrightnessError> {
    Writer::new().set(percent)
}
