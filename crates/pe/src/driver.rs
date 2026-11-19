//! The IASIO COM object.

use crate::asio::*;
use crate::unixlib;
use crate::{
    CLSID_RUSTASIO, DRIVER_NAME, E_NOINTERFACE, E_POINTER, IID_IUNKNOWN, OBJECTS, S_OK, guid_eq,
};
use rustasio_abi::*;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::*;
use windows_sys::core::{GUID, HRESULT};

static OUTPUT_READY_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static POS_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn log_once(flag: &std::sync::atomic::AtomicBool, msg: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        crate::log(msg);
    }
}

const SUPPORTED_RATES: [f64; 6] = [44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];

type This = *mut Driver;

#[repr(C)]
struct AsioVtbl {
    query_interface: unsafe extern "system" fn(This, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(This) -> u32,
    release: unsafe extern "system" fn(This) -> u32,
    init: unsafe extern "system" fn(This, *mut c_void) -> AsioBool,
    get_driver_name: unsafe extern "system" fn(This, *mut u8),
    get_driver_version: unsafe extern "system" fn(This) -> i32,
    get_error_message: unsafe extern "system" fn(This, *mut u8),
    start: unsafe extern "system" fn(This) -> AsioError,
    stop: unsafe extern "system" fn(This) -> AsioError,
    get_channels: unsafe extern "system" fn(This, *mut i32, *mut i32) -> AsioError,
    get_latencies: unsafe extern "system" fn(This, *mut i32, *mut i32) -> AsioError,
    get_buffer_size:
        unsafe extern "system" fn(This, *mut i32, *mut i32, *mut i32, *mut i32) -> AsioError,
    can_sample_rate: unsafe extern "system" fn(This, AsioSampleRate) -> AsioError,
    get_sample_rate: unsafe extern "system" fn(This, *mut AsioSampleRate) -> AsioError,
    set_sample_rate: unsafe extern "system" fn(This, AsioSampleRate) -> AsioError,
    get_clock_sources: unsafe extern "system" fn(This, *mut AsioClockSource, *mut i32) -> AsioError,
    set_clock_source: unsafe extern "system" fn(This, i32) -> AsioError,
    get_sample_position: unsafe extern "system" fn(This, *mut Asio64, *mut Asio64) -> AsioError,
    get_channel_info: unsafe extern "system" fn(This, *mut AsioChannelInfo) -> AsioError,
    create_buffers: unsafe extern "system" fn(
        This,
        *mut AsioBufferInfo,
        i32,
        i32,
        *const AsioCallbacks,
    ) -> AsioError,
    dispose_buffers: unsafe extern "system" fn(This) -> AsioError,
    control_panel: unsafe extern "system" fn(This) -> AsioError,
    future: unsafe extern "system" fn(This, i32, *mut c_void) -> AsioError,
    output_ready: unsafe extern "system" fn(This) -> AsioError,
}

static VTBL: AsioVtbl = AsioVtbl {
    query_interface,
    add_ref,
    release,
    init,
    get_driver_name,
    get_driver_version,
    get_error_message,
    start,
    stop,
    get_channels,
    get_latencies,
    get_buffer_size,
    can_sample_rate,
    get_sample_rate,
    set_sample_rate,
    get_clock_sources,
    set_clock_source,
    get_sample_position,
    get_channel_info,
    create_buffers,
    dispose_buffers,
    control_panel,
    future,
    output_ready,
};

struct Buffers {
    frames: u32,
    /// per output channel: two halves of `frames` samples (4 bytes each)
    mem: Vec<Option<Box<[u32]>>>,
    callbacks: *const AsioCallbacks,
    time_info: bool,
}

struct Inner {
    initialized: bool,
    cfg: Config,
    rate: f64,
    error: String,
    buffers: Option<Buffers>,
    running: bool,
    thread: HANDLE,
    thread_id: u32,
}

unsafe impl Send for Inner {}

#[repr(C)]
pub struct Driver {
    vtbl: &'static AsioVtbl,
    refs: AtomicU32,
    inner: Mutex<Inner>,
}

impl Driver {
    pub fn new() -> This {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        Box::into_raw(Box::new(Driver {
            vtbl: &VTBL,
            refs: AtomicU32::new(1),
            inner: Mutex::new(Inner {
                initialized: false,
                cfg: Config::default(),
                rate: 48000.0,
                error: String::new(),
                buffers: None,
                running: false,
                thread: std::ptr::null_mut(),
                thread_id: 0,
            }),
        }))
    }
}

unsafe fn inner<'a>(this: This) -> std::sync::MutexGuard<'a, Inner> {
    unsafe { (*this).inner.lock().unwrap_or_else(|e| e.into_inner()) }
}

impl Inner {
    fn sample_type(&self) -> i32 {
        if self.cfg.sample_format == FMT_I32 {
            ASIOST_INT32_LSB
        } else {
            ASIOST_FLOAT32_LSB
        }
    }

    /// Stop the stream; returns the callback thread to join. Joining must happen
    /// outside the lock: the host may call into the driver from bufferSwitch.
    fn begin_stop(&mut self) -> Option<(HANDLE, u32)> {
        if !self.running {
            return None;
        }
        self.running = false;
        unixlib::call0(Func::Stop);
        let t = (self.thread, self.thread_id);
        self.thread = std::ptr::null_mut();
        self.thread_id = 0;
        if t.0.is_null() { None } else { Some(t) }
    }
}

fn join(t: Option<(HANDLE, u32)>) {
    let Some((h, tid)) = t else { return };
    unsafe {
        // Never join ourselves (host calling stop() from bufferSwitch).
        if GetCurrentThreadId() != tid && WaitForSingleObject(h, 2000) != WAIT_OBJECT_0 {
            crate::log!("callback thread did not exit in time");
        }
        CloseHandle(h);
    }
}

unsafe fn do_stop(this: This) {
    let t = unsafe { inner(this).begin_stop() };
    join(t);
}

unsafe fn do_dispose(this: This) {
    unsafe {
        do_stop(this);
        if inner(this).buffers.take().is_some() {
            unixlib::call0(Func::DisposeBuffers);
        }
    }
}

// ---------------------------------------------------------------------------
// IUnknown

pub unsafe extern "system" fn query_interface(
    this: This,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe {
        if guid_eq(riid, &IID_IUNKNOWN) || guid_eq(riid, &CLSID_RUSTASIO) {
            add_ref(this);
            *ppv = this as *mut c_void;
            return S_OK;
        }
        *ppv = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

unsafe extern "system" fn add_ref(this: This) -> u32 {
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

pub unsafe extern "system" fn release(this: This) -> u32 {
    unsafe {
        let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if n == 0 {
            do_dispose(this);
            {
                let g = inner(this);
                if g.initialized {
                    unixlib::call0(Func::Exit);
                }
            }
            drop(Box::from_raw(this));
            OBJECTS.fetch_sub(1, Ordering::AcqRel);
        }
        n
    }
}

// ---------------------------------------------------------------------------
// IASIO

unsafe extern "system" fn init(this: This, _sys: *mut c_void) -> AsioBool {
    unsafe {
        let mut g = inner(this);
        if g.initialized {
            return ASIO_TRUE;
        }
        if !unixlib::available() {
            g.error = "unix library rustasio64.so not loaded (is the DLL marked builtin / \
                       WINEDLLPATH set?)"
                .into();
            report_init_error(&g.error);
            return ASIO_FALSE;
        }
        let mut p = InitParams::default();
        if unixlib::call(Func::Init, &mut p) != STATUS_OK {
            g.error = match p.error_str() {
                "" => "PipeWire initialization failed".into(),
                e => e.into(),
            };
            report_init_error(&g.error);
            return ASIO_FALSE;
        }
        g.cfg = p.config;
        g.rate = p.config.sample_rate as f64;
        g.initialized = true;
        crate::log!("init ok: {:?}", g.cfg);
        ASIO_TRUE
    }
}

static INIT_ERROR_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Many hosts ignore getErrorMessage and only complain about missing channels,
/// so show the real reason once per process. The box runs on its own thread:
/// init() must not block on the user, and the host may hold its UI locks.
fn report_init_error(msg: &str) {
    crate::log(&format!("init failed: {msg}"));
    if INIT_ERROR_SHOWN.swap(true, Ordering::Relaxed)
        || std::env::var_os("RUSTASIO_NO_ERROR_DIALOG").is_some()
    {
        return;
    }
    let text: Vec<u16> = format!("RustASIO could not start:\n\n{msg}")
        .encode_utf16()
        .chain([0])
        .collect();
    let _ = std::thread::Builder::new()
        .name("rustasio-error".into())
        .spawn(move || unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let title: Vec<u16> = "RustASIO".encode_utf16().chain([0]).collect();
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            );
        });
}

unsafe extern "system" fn get_driver_name(_this: This, name: *mut u8) {
    unsafe { copy_cstr(name, 32, DRIVER_NAME) };
}

unsafe extern "system" fn get_driver_version(_this: This) -> i32 {
    1
}

unsafe extern "system" fn get_error_message(this: This, s: *mut u8) {
    unsafe {
        let g = inner(this);
        copy_cstr(s, 124, &g.error);
    }
}

struct ThreadCtx {
    callbacks: *const AsioCallbacks,
    time_info: bool,
    rate: f64,
}

unsafe extern "system" fn callback_thread(param: *mut c_void) -> u32 {
    unsafe {
        let ctx = Box::from_raw(param as *mut ThreadCtx);
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let cb = &*ctx.callbacks;
        let mut time: AsioTime = std::mem::zeroed();
        let mut warned_rate = false;
        loop {
            let mut w = WaitParams::default();
            unixlib::call(Func::Wait, &mut w);
            if w.status != WAIT_RUN {
                break;
            }
            if !warned_rate && w.sample_rate != 0 && w.sample_rate as f64 != ctx.rate {
                warned_rate = true;
                crate::log!(
                    "graph runs at {} Hz but ASIO rate is {} Hz",
                    w.sample_rate,
                    ctx.rate
                );
            }
            let half = w.half as i32;
            match (ctx.time_info, cb.buffer_switch_time_info, cb.buffer_switch) {
                (true, Some(f), _) => {
                    time.time_info = AsioTimeInfo {
                        speed: 1.0,
                        system_time: w.system_ns.into(),
                        sample_position: w.sample_pos.into(),
                        sample_rate: ctx.rate,
                        flags: K_SYSTEM_TIME_VALID
                            | K_SAMPLE_POSITION_VALID
                            | K_SAMPLE_RATE_VALID
                            | K_SPEED_VALID,
                        reserved: [0; 12],
                    };
                    f(&mut time, half, ASIO_TRUE);
                }
                (_, _, Some(f)) => f(half, ASIO_TRUE),
                _ => {}
            }
            unixlib::call0(Func::Done);
        }
        crate::log!("callback thread exit");
        0
    }
}

unsafe extern "system" fn start(this: This) -> AsioError {
    unsafe {
        let mut g = inner(this);
        if g.running {
            return ASE_OK;
        }
        let Some(b) = &g.buffers else {
            return ASE_INVALID_MODE;
        };
        let ctx = Box::into_raw(Box::new(ThreadCtx {
            callbacks: b.callbacks,
            time_info: b.time_info,
            rate: g.rate,
        }));
        if unixlib::call0(Func::Start) != STATUS_OK {
            drop(Box::from_raw(ctx));
            g.error = "PipeWire stream failed to start".into();
            return ASE_HW_MALFUNCTION;
        }
        let mut tid = 0u32;
        let h = CreateThread(
            std::ptr::null(),
            0,
            Some(callback_thread),
            ctx as *const c_void,
            0,
            &mut tid,
        );
        if h.is_null() {
            drop(Box::from_raw(ctx));
            unixlib::call0(Func::Stop);
            return ASE_HW_MALFUNCTION;
        }
        g.thread = h;
        g.thread_id = tid;
        g.running = true;
        crate::log!("start");
        ASE_OK
    }
}

unsafe extern "system" fn stop(this: This) -> AsioError {
    unsafe { do_stop(this) };
    ASE_OK
}

unsafe extern "system" fn get_channels(this: This, ins: *mut i32, outs: *mut i32) -> AsioError {
    unsafe {
        let g = inner(this);
        if !g.initialized {
            return ASE_NOT_PRESENT;
        }
        if !ins.is_null() {
            *ins = g.cfg.n_inputs as i32;
        }
        if !outs.is_null() {
            *outs = g.cfg.n_outputs as i32;
        }
    }
    ASE_OK
}

unsafe extern "system" fn get_latencies(this: This, inl: *mut i32, outl: *mut i32) -> AsioError {
    unsafe {
        let g = inner(this);
        let frames = g
            .buffers
            .as_ref()
            .map(|b| b.frames)
            .unwrap_or(g.cfg.buffer_size) as i32;
        // ring: one ASIO buffer + headroom + one graph quantum; sync: same cycle
        let q = g.cfg.quantum as i32;
        let out = if g.cfg.sync != 0 {
            frames
        } else {
            frames + (g.cfg.headroom as i32).max(q) + q
        };
        if !inl.is_null() {
            *inl = frames;
        }
        if !outl.is_null() {
            *outl = out;
        }
    }
    ASE_OK
}

unsafe extern "system" fn get_buffer_size(
    this: This,
    min: *mut i32,
    max: *mut i32,
    pref: *mut i32,
    gran: *mut i32,
) -> AsioError {
    unsafe {
        let g = inner(this);
        let n = g.cfg.buffer_size as i32;
        // FMOD uses its own DSP block size when it fits [min,max] and the
        // granularity; otherwise it adapts internally and renders whole blocks
        // inside some callbacks (bursty). Sync mode needs buffer == quantum.
        let fixed = g.cfg.fixed_buffer != 0 || g.cfg.sync != 0;
        let (lo, hi, gr) = if fixed { (n, n, 0) } else { (16, 4096, -1) };
        for (p, v) in [(min, lo), (max, hi), (pref, n), (gran, gr)] {
            if !p.is_null() {
                *p = v;
            }
        }
        crate::log!("getBufferSize -> min={lo} max={hi} pref={n} gran={gr}");
    }
    ASE_OK
}

unsafe extern "system" fn can_sample_rate(_this: This, rate: AsioSampleRate) -> AsioError {
    crate::log!("canSampleRate({rate})");
    if SUPPORTED_RATES.contains(&rate) {
        ASE_OK
    } else {
        ASE_NO_CLOCK
    }
}

unsafe extern "system" fn get_sample_rate(this: This, rate: *mut AsioSampleRate) -> AsioError {
    if rate.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    unsafe { *rate = inner(this).rate };
    ASE_OK
}

unsafe extern "system" fn set_sample_rate(this: This, rate: AsioSampleRate) -> AsioError {
    unsafe {
        let mut g = inner(this);
        if rate == 0.0 || rate == g.rate {
            return ASE_OK;
        }
        if !SUPPORTED_RATES.contains(&rate) {
            return ASE_NO_CLOCK;
        }
        let mut p = SetRateParams {
            sample_rate: rate as u32,
        };
        unixlib::call(Func::SetRate, &mut p);
        g.rate = rate;
        crate::log!("setSampleRate({rate})");
    }
    ASE_OK
}

unsafe extern "system" fn get_clock_sources(
    _this: This,
    clocks: *mut AsioClockSource,
    num: *mut i32,
) -> AsioError {
    unsafe {
        if clocks.is_null() || num.is_null() {
            return ASE_INVALID_PARAMETER;
        }
        if *num >= 1 {
            let c = &mut *clocks;
            c.index = 0;
            c.associated_channel = -1;
            c.associated_group = -1;
            c.is_current_source = ASIO_TRUE;
            copy_cstr(c.name.as_mut_ptr(), 32, "Internal");
        }
        *num = 1;
    }
    ASE_OK
}

unsafe extern "system" fn set_clock_source(_this: This, reference: i32) -> AsioError {
    if reference == 0 {
        ASE_OK
    } else {
        ASE_INVALID_MODE
    }
}

unsafe extern "system" fn get_sample_position(
    this: This,
    pos: *mut Asio64,
    ts: *mut Asio64,
) -> AsioError {
    log_once(&POS_LOGGED, "getSamplePosition called");
    unsafe {
        if !inner(this).running {
            return ASE_SP_NOT_ADVANCING;
        }
        let mut p = PositionParams::default();
        unixlib::call(Func::GetPosition, &mut p);
        if !pos.is_null() {
            *pos = p.sample_pos.into();
        }
        if !ts.is_null() {
            *ts = p.system_ns.into();
        }
    }
    ASE_OK
}

unsafe extern "system" fn get_channel_info(this: This, info: *mut AsioChannelInfo) -> AsioError {
    unsafe {
        if info.is_null() {
            return ASE_INVALID_PARAMETER;
        }
        crate::log!(
            "getChannelInfo(ch={} input={})",
            (*info).channel,
            (*info).is_input
        );
        let g = inner(this);
        let i = &mut *info;
        let count = if i.is_input != 0 {
            g.cfg.n_inputs
        } else {
            g.cfg.n_outputs
        } as i32;
        if i.channel < 0 || i.channel >= count {
            return ASE_INVALID_PARAMETER;
        }
        i.is_active = g
            .buffers
            .as_ref()
            .map(|b| i.is_input == 0 && b.mem.get(i.channel as usize).is_some_and(|m| m.is_some()))
            .unwrap_or(false) as AsioBool;
        i.channel_group = 0;
        i.sample_type = g.sample_type();
        let prefix = if i.is_input != 0 { "In" } else { "Out" };
        copy_cstr(
            i.name.as_mut_ptr(),
            32,
            &format!("{prefix} {}", i.channel + 1),
        );
    }
    ASE_OK
}

unsafe extern "system" fn create_buffers(
    this: This,
    infos: *mut AsioBufferInfo,
    num: i32,
    size: i32,
    callbacks: *const AsioCallbacks,
) -> AsioError {
    unsafe {
        if !inner(this).initialized {
            return ASE_NOT_PRESENT;
        }
        if infos.is_null() || callbacks.is_null() || num <= 0 || !(16..=8192).contains(&size) {
            return ASE_INVALID_PARAMETER;
        }
        do_dispose(this);
        let mut g = inner(this);

        let infos = std::slice::from_raw_parts_mut(infos, num as usize);
        let nout = g.cfg.n_outputs as usize;
        for bi in infos.iter() {
            if bi.is_input != 0 || bi.channel_num < 0 || bi.channel_num as usize >= nout {
                g.error = format!(
                    "invalid channel request (input={} ch={})",
                    bi.is_input, bi.channel_num
                );
                return ASE_INVALID_PARAMETER;
            }
        }

        let frames = size as u32;
        let mut mem: Vec<Option<Box<[u32]>>> = (0..nout).map(|_| None).collect();
        let mut p = CreateBuffersParams {
            frames,
            n_outputs: nout as u32,
            n_inputs: 0,
            sample_format: g.cfg.sample_format,
            outputs: [[0; 2]; MAX_CHANNELS],
            inputs: [[0; 2]; MAX_CHANNELS],
        };
        for bi in infos.iter_mut() {
            let ch = bi.channel_num as usize;
            let buf =
                mem[ch].get_or_insert_with(|| vec![0u32; 2 * frames as usize].into_boxed_slice());
            let base = buf.as_mut_ptr();
            let halves = [base, base.add(frames as usize)];
            bi.buffers = [halves[0] as *mut c_void, halves[1] as *mut c_void];
            p.outputs[ch] = [halves[0] as u64, halves[1] as u64];
        }

        // Does the host want bufferSwitchTimeInfo?
        let cb = &*callbacks;
        let time_info = match cb.asio_message {
            Some(m) => {
                m(
                    K_ASIO_SELECTOR_SUPPORTED,
                    K_ASIO_SUPPORTS_TIME_INFO,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 1
                    && m(
                        K_ASIO_SUPPORTS_TIME_INFO,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    ) == 1
            }
            None => false,
        } && cb.buffer_switch_time_info.is_some();

        if unixlib::call(Func::CreateBuffers, &mut p) != STATUS_OK {
            g.error = "PipeWire filter creation failed".into();
            return ASE_HW_MALFUNCTION;
        }
        crate::log!("createBuffers: {num} channels x {frames} frames, timeInfo={time_info}");
        g.buffers = Some(Buffers {
            frames,
            mem,
            callbacks,
            time_info,
        });
    }
    ASE_OK
}

unsafe extern "system" fn dispose_buffers(this: This) -> AsioError {
    unsafe { do_dispose(this) };
    ASE_OK
}

unsafe extern "system" fn control_panel(_this: This) -> AsioError {
    ASE_NOT_PRESENT
}

unsafe extern "system" fn future(_this: This, selector: i32, opt: *mut c_void) -> AsioError {
    crate::log!("future({selector:#x})");
    match selector {
        K_ASIO_CAN_TIME_INFO => ASE_SUCCESS,
        K_ASIO_CAN_DO_IO_FORMAT | K_ASIO_SET_IO_FORMAT | K_ASIO_GET_IO_FORMAT => {
            if opt.is_null() {
                return ASE_INVALID_PARAMETER;
            }
            let fmt = opt as *mut i32;
            unsafe {
                if selector == K_ASIO_GET_IO_FORMAT {
                    *fmt = K_ASIO_PCM_FORMAT;
                    ASE_SUCCESS
                } else if *fmt == K_ASIO_PCM_FORMAT {
                    ASE_SUCCESS
                } else {
                    ASE_NO_CLOCK
                }
            }
        }
        _ => ASE_INVALID_PARAMETER,
    }
}

unsafe extern "system" fn output_ready(_this: This) -> AsioError {
    log_once(&OUTPUT_READY_LOGGED, "outputReady called");
    ASE_NOT_PRESENT
}
