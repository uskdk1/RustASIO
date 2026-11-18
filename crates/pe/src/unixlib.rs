//! Bridge to the unix half via Wine's unixlib mechanism (Wine >= 7, PE/unix split).
//!
//! Equivalent of winecrt0's `__wine_init_unix_call` + `__wine_unix_call`.

use rustasio_abi::{Func, LogParams, STATUS_FAIL};
use std::ffi::c_void;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

type Dispatcher = unsafe extern "system" fn(u64, u32, *mut c_void) -> i32;
type NtQueryVirtualMemory =
    unsafe extern "system" fn(HANDLE, *const c_void, u32, *mut c_void, usize, *mut usize) -> i32;

/// MEMORY_INFORMATION_CLASS::MemoryWineUnixFuncs
const MEMORY_WINE_UNIX_FUNCS: u32 = 1000;

struct UnixLib {
    handle: u64,
    dispatch: Dispatcher,
}

static LIB: OnceLock<Option<UnixLib>> = OnceLock::new();

fn load() -> Option<UnixLib> {
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr() as _);
        if ntdll.is_null() {
            return None;
        }
        let q = GetProcAddress(ntdll, c"NtQueryVirtualMemory".as_ptr() as _)?;
        let q: NtQueryVirtualMemory = std::mem::transmute(q);
        // `__wine_unix_call_dispatcher` is a data export: a pointer to the function.
        let d = GetProcAddress(ntdll, c"__wine_unix_call_dispatcher".as_ptr() as _)?;
        let dispatch = *(d as *const Option<Dispatcher>);
        let dispatch = dispatch?;
        let mut handle: u64 = 0;
        let status = q(
            -1isize as HANDLE, // GetCurrentProcess()
            crate::module() as *const c_void,
            MEMORY_WINE_UNIX_FUNCS,
            &mut handle as *mut u64 as *mut c_void,
            std::mem::size_of::<u64>(),
            std::ptr::null_mut(),
        );
        if status != 0 || handle == 0 {
            return None;
        }
        Some(UnixLib { handle, dispatch })
    }
}

/// True once the unix half is loaded.
pub fn available() -> bool {
    LIB.get_or_init(load).is_some()
}

pub fn call<T>(f: Func, params: &mut T) -> i32 {
    match LIB.get_or_init(load) {
        Some(l) => unsafe { (l.dispatch)(l.handle, f as u32, params as *mut T as *mut c_void) },
        None => STATUS_FAIL,
    }
}

pub fn call0(f: Func) -> i32 {
    call(f, &mut [0u64; 1])
}

pub fn log(msg: &str) {
    let mut p = LogParams {
        ptr: msg.as_ptr() as u64,
        len: msg.len() as u64,
    };
    call(Func::Log, &mut p);
}
