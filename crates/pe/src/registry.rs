//! DllRegisterServer / DllUnregisterServer: COM class + ASIO driver list entry.

use crate::{DRIVER_NAME, module};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Registry::*;

const CLSID_STR: &str = "{7C1B3E4A-5D2F-4E8B-9A61-3F0C2D9E8B57}";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_value(root: HKEY, path: &str, name: Option<&str>, value: &[u16]) -> Result<(), u32> {
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let r = RegCreateKeyExW(
            root,
            wide(path).as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if r != 0 {
            return Err(r);
        }
        let name_w = name.map(wide);
        let r = RegSetValueExW(
            key,
            name_w
                .as_ref()
                .map(|n| n.as_ptr())
                .unwrap_or(std::ptr::null()),
            0,
            REG_SZ,
            value.as_ptr() as *const u8,
            (value.len() * 2) as u32,
        );
        RegCloseKey(key);
        if r != 0 { Err(r) } else { Ok(()) }
    }
}

fn module_path() -> Vec<u16> {
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(module(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    buf.truncate(n);
    buf.push(0);
    buf
}

pub fn register() -> Result<(), u32> {
    let clsid_key = format!("CLSID\\{CLSID_STR}");
    set_value(
        HKEY_CLASSES_ROOT,
        &clsid_key,
        None,
        &wide("RustASIO Object"),
    )?;
    let inproc = format!("{clsid_key}\\InprocServer32");
    set_value(HKEY_CLASSES_ROOT, &inproc, None, &module_path())?;
    set_value(
        HKEY_CLASSES_ROOT,
        &inproc,
        Some("ThreadingModel"),
        &wide("Apartment"),
    )?;

    let asio_key = format!("Software\\ASIO\\{DRIVER_NAME}");
    set_value(
        HKEY_LOCAL_MACHINE,
        &asio_key,
        Some("CLSID"),
        &wide(CLSID_STR),
    )?;
    set_value(
        HKEY_LOCAL_MACHINE,
        &asio_key,
        Some("Description"),
        &wide(DRIVER_NAME),
    )?;
    Ok(())
}

pub fn unregister() {
    unsafe {
        RegDeleteTreeW(
            HKEY_CLASSES_ROOT,
            wide(&format!("CLSID\\{CLSID_STR}")).as_ptr(),
        );
        RegDeleteTreeW(
            HKEY_LOCAL_MACHINE,
            wide(&format!("Software\\ASIO\\{DRIVER_NAME}")).as_ptr(),
        );
    }
}
