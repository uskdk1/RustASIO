//! PE half of RustASIO: an in-process COM server implementing IASIO.
//! Built for x86_64-pc-windows-gnu and marked as a Wine builtin so that Wine
//! loads `rustasio64.so` next to it; all real work happens there.
#![cfg(windows)]

mod asio;
mod driver;
mod registry;
mod unixlib;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::core::{BOOL, GUID, HRESULT};

/// {7C1B3E4A-5D2F-4E8B-9A61-3F0C2D9E8B57}
pub const CLSID_RUSTASIO: GUID = GUID::from_u128(0x7C1B3E4A_5D2F_4E8B_9A61_3F0C2D9E8B57);
pub const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);
pub const IID_ICLASSFACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_C000_000000000046);

pub const DRIVER_NAME: &str = "RustASIO";

pub const S_OK: HRESULT = 0;
pub const S_FALSE: HRESULT = 1;
pub const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as i32;
pub const E_POINTER: HRESULT = 0x8000_4003u32 as i32;
pub const E_OUTOFMEMORY: HRESULT = 0x8007_000Eu32 as i32;
pub const CLASS_E_NOAGGREGATION: HRESULT = 0x8004_0110u32 as i32;
pub const CLASS_E_CLASSNOTAVAILABLE: HRESULT = 0x8004_0111u32 as i32;
pub const SELFREG_E_CLASS: HRESULT = 0x8004_0201u32 as i32;

static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// Live objects + server locks, for DllCanUnloadNow.
pub static OBJECTS: AtomicU32 = AtomicU32::new(0);

pub fn module() -> HMODULE {
    MODULE.load(Ordering::Relaxed)
}

pub(crate) fn guid_eq(a: *const GUID, b: &GUID) -> bool {
    if a.is_null() {
        return false;
    }
    let a = unsafe { &*a };
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// Log through the unix side (stderr, only when `log = true`).
pub fn log(msg: &str) {
    unixlib::log(msg);
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log(&format!($($t)*)) };
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst, Ordering::Relaxed);
        unsafe { DisableThreadLibraryCalls(hinst) };
    }
    TRUE
}

// ---------------------------------------------------------------------------
// IClassFactory

#[repr(C)]
struct ClassFactoryVtbl {
    query_interface:
        unsafe extern "system" fn(*mut ClassFactory, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    release: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    create_instance: unsafe extern "system" fn(
        *mut ClassFactory,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    lock_server: unsafe extern "system" fn(*mut ClassFactory, BOOL) -> HRESULT,
}

#[repr(C)]
struct ClassFactory {
    vtbl: &'static ClassFactoryVtbl,
}

unsafe impl Sync for ClassFactory {}

static FACTORY_VTBL: ClassFactoryVtbl = ClassFactoryVtbl {
    query_interface: cf_query_interface,
    add_ref: cf_add_ref,
    release: cf_release,
    create_instance: cf_create_instance,
    lock_server: cf_lock_server,
};

static FACTORY: ClassFactory = ClassFactory { vtbl: &FACTORY_VTBL };

unsafe extern "system" fn cf_query_interface(
    this: *mut ClassFactory,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe {
        if guid_eq(riid, &IID_IUNKNOWN) || guid_eq(riid, &IID_ICLASSFACTORY) {
            *ppv = this as *mut c_void;
            return S_OK;
        }
        *ppv = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

unsafe extern "system" fn cf_add_ref(_this: *mut ClassFactory) -> u32 {
    2
}

unsafe extern "system" fn cf_release(_this: *mut ClassFactory) -> u32 {
    1
}

unsafe extern "system" fn cf_create_instance(
    _this: *mut ClassFactory,
    outer: *mut c_void,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe {
        *ppv = std::ptr::null_mut();
        if !outer.is_null() {
            return CLASS_E_NOAGGREGATION;
        }
        let obj = driver::Driver::new();
        // Hosts ask for the driver CLSID itself as the interface id.
        let hr = driver::query_interface(obj, riid, ppv);
        driver::release(obj);
        hr
    }
}

unsafe extern "system" fn cf_lock_server(_this: *mut ClassFactory, lock: BOOL) -> HRESULT {
    if lock != 0 {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
    } else {
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
    S_OK
}

// ---------------------------------------------------------------------------
// DLL exports

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe {
        *ppv = std::ptr::null_mut();
        if !guid_eq(rclsid, &CLSID_RUSTASIO) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        cf_query_interface(&FACTORY as *const ClassFactory as *mut ClassFactory, riid, ppv)
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::Acquire) == 0 { S_OK } else { S_FALSE }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => S_OK,
        Err(_) => SELFREG_E_CLASS,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    registry::unregister();
    S_OK
}
