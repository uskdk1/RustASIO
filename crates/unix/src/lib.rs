//! Unix half of RustASIO, loaded by Wine next to the builtin PE `rustasio64.dll`.
//! Exposes `__wine_unix_call_funcs`; see `rustasio-abi` for the call contract.

pub mod config;
pub mod engine;

use rustasio_abi::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

static LOG: AtomicBool = AtomicBool::new(false);

pub fn log_enabled() -> bool {
    LOG.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {
        if $crate::log_enabled() {
            eprintln!("[rustasio] {}", format_args!($($t)*));
        }
    };
}

type Entry = unsafe extern "C" fn(*mut c_void) -> i32;

#[unsafe(no_mangle)]
#[used]
pub static __wine_unix_call_funcs: [Entry; FUNC_COUNT] = [
    unix_init,
    unix_exit,
    unix_create_buffers,
    unix_dispose_buffers,
    unix_start,
    unix_stop,
    unix_wait,
    unix_done,
    unix_get_position,
    unix_set_rate,
    unix_log,
];

unsafe extern "C" fn unix_init(args: *mut c_void) -> i32 {
    (|| {
        let p = unsafe { &mut *(args as *mut InitParams) };
        let mut g = engine::ENGINE.lock().unwrap();
        if g.is_none() {
            let s = config::Settings::load();
            LOG.store(s.log, Ordering::Relaxed);
            log!(
                "config: {:?} (file: {:?})",
                s,
                config::Settings::config_path()
            );
            match engine::Engine::new(s) {
                Ok(e) => *g = Some(e),
                Err(e) => {
                    eprintln!("[rustasio] init failed: {e}");
                    return STATUS_FAIL;
                }
            }
        }
        let s = &g.as_ref().unwrap().settings;
        p.config = Config {
            sample_rate: s.sample_rate,
            buffer_size: s.buffer_size,
            n_outputs: s.outputs,
            n_inputs: 0,
            sample_format: s.sample_format,
            sync: s.sync as u32,
            log: s.log as u32,
            headroom: s.headroom,
            quantum: s.quantum,
            fixed_buffer: s.fixed_buffer as u32,
            _pad: 0,
        };
        STATUS_OK
    })()
}

unsafe extern "C" fn unix_exit(_args: *mut c_void) -> i32 {
    {
        let e = engine::ENGINE.lock().unwrap().take();
        drop(e);
        log!("exit");
        STATUS_OK
    }
}

unsafe extern "C" fn unix_create_buffers(args: *mut c_void) -> i32 {
    (|| {
        let p = unsafe { &*(args as *const CreateBuffersParams) };
        if p.frames == 0 || p.n_outputs as usize > MAX_CHANNELS {
            return STATUS_INVALID;
        }
        let mut g = engine::ENGINE.lock().unwrap();
        let Some(e) = g.as_mut() else {
            return STATUS_FAIL;
        };
        match e.create_buffers(p) {
            Ok(()) => STATUS_OK,
            Err(err) => {
                eprintln!("[rustasio] createBuffers: {err}");
                STATUS_FAIL
            }
        }
    })()
}

unsafe extern "C" fn unix_dispose_buffers(_args: *mut c_void) -> i32 {
    {
        if let Some(e) = engine::ENGINE.lock().unwrap().as_mut() {
            e.dispose_buffers();
        }
        STATUS_OK
    }
}

unsafe extern "C" fn unix_start(_args: *mut c_void) -> i32 {
    (|| {
        let mut g = engine::ENGINE.lock().unwrap();
        let Some(e) = g.as_mut() else {
            return STATUS_FAIL;
        };
        match e.start() {
            Ok(()) => STATUS_OK,
            Err(err) => {
                eprintln!("[rustasio] start: {err}");
                STATUS_FAIL
            }
        }
    })()
}

unsafe extern "C" fn unix_stop(_args: *mut c_void) -> i32 {
    {
        if let Some(e) = engine::ENGINE.lock().unwrap().as_mut() {
            e.stop();
        }
        STATUS_OK
    }
}

unsafe extern "C" fn unix_wait(args: *mut c_void) -> i32 {
    let p = unsafe { &mut *(args as *mut WaitParams) };
    engine::wait(p);
    STATUS_OK
}

unsafe extern "C" fn unix_done(_args: *mut c_void) -> i32 {
    engine::done();
    STATUS_OK
}

unsafe extern "C" fn unix_get_position(args: *mut c_void) -> i32 {
    let p = unsafe { &mut *(args as *mut PositionParams) };
    let sh = &engine::SHARED;
    p.sample_pos = sh.req_pos.load(Ordering::Relaxed);
    p.system_ns = sh.req_ns.load(Ordering::Relaxed);
    STATUS_OK
}

unsafe extern "C" fn unix_set_rate(args: *mut c_void) -> i32 {
    (|| {
        let p = unsafe { &*(args as *const SetRateParams) };
        let mut g = engine::ENGINE.lock().unwrap();
        let Some(e) = g.as_mut() else {
            return STATUS_FAIL;
        };
        e.set_rate(p.sample_rate);
        log!("sample rate -> {}", p.sample_rate);
        STATUS_OK
    })()
}

unsafe extern "C" fn unix_log(args: *mut c_void) -> i32 {
    let p = unsafe { &*(args as *const LogParams) };
    if log_enabled() && p.ptr != 0 {
        let b = unsafe { std::slice::from_raw_parts(p.ptr as *const u8, p.len as usize) };
        eprintln!("[rustasio:pe] {}", String::from_utf8_lossy(b));
    }
    STATUS_OK
}
