//! Minimal ASIO host for testing RustASIO under Wine/Proton.
//!   asiotest [seconds]   — list ASIO drivers, open RustASIO, play a quiet 440 Hz sine.

#[cfg(not(windows))]
fn main() {
    eprintln!("asiotest is a Windows program; build with --target x86_64-pc-windows-gnu");
}

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering::*};
    use std::time::Instant;
    use windows_sys::Win32::System::Com::*;
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::core::GUID;

    const CLSID: GUID = GUID::from_u128(0x7C1B3E4A_5D2F_4E8B_9A61_3F0C2D9E8B57);

    type This = *mut *const Vtbl;
    #[repr(C)]
    struct Vtbl {
        qi: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(This) -> u32,
        init: unsafe extern "system" fn(This, *mut c_void) -> i32,
        get_driver_name: unsafe extern "system" fn(This, *mut u8),
        get_driver_version: unsafe extern "system" fn(This) -> i32,
        get_error_message: unsafe extern "system" fn(This, *mut u8),
        start: unsafe extern "system" fn(This) -> i32,
        stop: unsafe extern "system" fn(This) -> i32,
        get_channels: unsafe extern "system" fn(This, *mut i32, *mut i32) -> i32,
        get_latencies: unsafe extern "system" fn(This, *mut i32, *mut i32) -> i32,
        get_buffer_size: unsafe extern "system" fn(This, *mut i32, *mut i32, *mut i32, *mut i32) -> i32,
        can_sample_rate: unsafe extern "system" fn(This, f64) -> i32,
        get_sample_rate: unsafe extern "system" fn(This, *mut f64) -> i32,
        set_sample_rate: unsafe extern "system" fn(This, f64) -> i32,
        get_clock_sources: usize,
        set_clock_source: usize,
        get_sample_position: unsafe extern "system" fn(This, *mut [u32; 2], *mut [u32; 2]) -> i32,
        get_channel_info: unsafe extern "system" fn(This, *mut ChannelInfo) -> i32,
        create_buffers: unsafe extern "system" fn(This, *mut BufferInfo, i32, i32, *const Callbacks) -> i32,
        dispose_buffers: unsafe extern "system" fn(This) -> i32,
        control_panel: usize,
        future: usize,
        output_ready: usize,
    }

    #[repr(C)]
    struct ChannelInfo {
        channel: i32,
        is_input: i32,
        is_active: i32,
        group: i32,
        ty: i32,
        name: [u8; 32],
    }

    #[repr(C)]
    struct BufferInfo {
        is_input: i32,
        channel: i32,
        buffers: [*mut c_void; 2],
    }

    #[repr(C)]
    struct Callbacks {
        buffer_switch: unsafe extern "C" fn(i32, i32),
        rate_changed: unsafe extern "C" fn(f64),
        message: unsafe extern "C" fn(i32, i32, *mut c_void, *mut f64) -> i32,
        buffer_switch_ti: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void,
    }

    struct Host {
        bufs: [[*mut c_void; 2]; 2],
        frames: usize,
        ty: i32,
        rate: f64,
    }
    static mut HOST: Host = Host { bufs: [[std::ptr::null_mut(); 2]; 2], frames: 0, ty: 19, rate: 48000.0 };
    static SWITCHES: AtomicU64 = AtomicU64::new(0);
    static LAST_US: AtomicU64 = AtomicU64::new(0);
    static MAX_GAP_US: AtomicU64 = AtomicU64::new(0);
    static PHASE_MILLI: AtomicI32 = AtomicI32::new(0);
    static mut PHASE: f64 = 0.0;
    static mut T0: Option<Instant> = None;

    unsafe fn render(index: i32) {
        unsafe {
            let h = &*std::ptr::addr_of!(HOST);
            let step = 440.0 * std::f64::consts::TAU / h.rate;
            for i in 0..h.frames {
                let v = PHASE.sin() * 0.05;
                PHASE = (PHASE + step) % std::f64::consts::TAU;
                for ch in &h.bufs {
                    let b = ch[index as usize];
                    if h.ty == 18 {
                        *(b as *mut i32).add(i) = (v * 2147483647.0) as i32;
                    } else {
                        *(b as *mut f32).add(i) = v as f32;
                    }
                }
            }
            PHASE_MILLI.store((PHASE * 1000.0) as i32, Relaxed);
            let now = (*std::ptr::addr_of!(T0)).unwrap().elapsed().as_micros() as u64;
            let last = LAST_US.swap(now, Relaxed);
            if last != 0 {
                MAX_GAP_US.fetch_max(now - last, Relaxed);
            }
            SWITCHES.fetch_add(1, Relaxed);
        }
    }

    unsafe extern "C" fn buffer_switch(index: i32, _direct: i32) {
        unsafe { render(index) }
    }
    unsafe extern "C" fn buffer_switch_ti(p: *mut c_void, index: i32, _direct: i32) -> *mut c_void {
        unsafe { render(index) };
        p
    }
    unsafe extern "C" fn rate_changed(r: f64) {
        println!("sampleRateDidChange({r})");
    }
    unsafe extern "C" fn message(sel: i32, val: i32, _m: *mut c_void, _o: *mut f64) -> i32 {
        match sel {
            1 => matches!(val, 1 | 2 | 3 | 4 | 7) as i32, // selector supported
            2 => 2,                                        // engine version
            7 => 1,                                        // supports time info
            _ => 0,
        }
    }

    fn cstr(b: &[u8]) -> String {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..n]).into_owned()
    }

    fn list_drivers() {
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            let path: Vec<u16> = "Software\\ASIO\0".encode_utf16().collect();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                println!("no HKLM\\Software\\ASIO key");
                return;
            }
            let mut i = 0;
            loop {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(key, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(),
                    std::ptr::null_mut(), std::ptr::null_mut()) != 0
                {
                    break;
                }
                println!("ASIO driver: {}", String::from_utf16_lossy(&name[..len as usize]));
                i += 1;
            }
            RegCloseKey(key);
        }
    }

    static mut LOGF: Option<std::fs::File> = None;
    macro_rules! println {
        ($($t:tt)*) => {{
            let s = format!($($t)*);
            std::io::Write::write_all(&mut std::io::stdout(), format!("{s}\n").as_bytes()).ok();
            #[allow(static_mut_refs)]
            if let Some(f) = unsafe { LOGF.as_mut() } { std::io::Write::write_all(f, format!("{s}\n").as_bytes()).ok(); }
        }};
    }

    pub fn main() {
        unsafe { LOGF = std::fs::File::create("C:\\asiotest.log").ok() };
        let secs: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
        list_drivers();
        unsafe {
            T0 = Some(Instant::now());
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let mut p: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(&CLSID, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &CLSID, &mut p);
            if hr != 0 || p.is_null() {
                println!("CoCreateInstance failed: {hr:#x}");
                std::process::exit(1);
            }
            let d = p as This;
            let v = &**d;
            let mut s = [0u8; 128];
            (v.get_driver_name)(d, s.as_mut_ptr());
            println!("driver: {} v{}", cstr(&s), (v.get_driver_version)(d));
            if (v.init)(d, std::ptr::null_mut()) == 0 {
                (v.get_error_message)(d, s.as_mut_ptr());
                println!("init failed: {}", cstr(&s));
                std::process::exit(1);
            }
            let (mut ni, mut no) = (0, 0);
            (v.get_channels)(d, &mut ni, &mut no);
            let (mut mn, mut mx, mut pref, mut gr) = (0, 0, 0, 0);
            (v.get_buffer_size)(d, &mut mn, &mut mx, &mut pref, &mut gr);
            let mut rate = 0.0;
            (v.get_sample_rate)(d, &mut rate);
            println!("channels in={ni} out={no}; buffer min={mn} max={mx} pref={pref} gran={gr}; rate={rate}");
            println!("canSampleRate(44100)={} (48000)={}", (v.can_sample_rate)(d, 44100.0), (v.can_sample_rate)(d, 48000.0));
            println!("setSampleRate(48000)={}", (v.set_sample_rate)(d, 48000.0));
            let mut ci = ChannelInfo { channel: 0, is_input: 0, is_active: 0, group: 0, ty: 0, name: [0; 32] };
            (v.get_channel_info)(d, &mut ci);
            println!("channel 0: type={} name={}", ci.ty, cstr(&ci.name));

            let mut infos = [
                BufferInfo { is_input: 0, channel: 0, buffers: [std::ptr::null_mut(); 2] },
                BufferInfo { is_input: 0, channel: 1, buffers: [std::ptr::null_mut(); 2] },
            ];
            static CB: Callbacks = Callbacks { buffer_switch, rate_changed, message, buffer_switch_ti };
            let r = (v.create_buffers)(d, infos.as_mut_ptr(), 2, pref, &CB);
            println!("createBuffers = {r}");
            if r != 0 {
                std::process::exit(1);
            }
            let h = &mut *std::ptr::addr_of_mut!(HOST);
            h.bufs = [infos[0].buffers, infos[1].buffers];
            h.frames = pref as usize;
            h.ty = ci.ty;
            h.rate = 48000.0;
            let (mut il, mut ol) = (0, 0);
            (v.get_latencies)(d, &mut il, &mut ol);
            println!("latencies in={il} out={ol}");

            println!("start = {}", (v.start)(d));
            let t = Instant::now();
            while t.elapsed().as_secs_f64() < secs {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let (mut sp, mut ts) = ([0u32; 2], [0u32; 2]);
                (v.get_sample_position)(d, &mut sp, &mut ts);
                let pos = ((sp[0] as u64) << 32) | sp[1] as u64;
                println!(
                    "switches={} pos={} max_gap={:.3}ms",
                    SWITCHES.load(Relaxed),
                    pos,
                    MAX_GAP_US.swap(0, Relaxed) as f64 / 1000.0
                );
            }
            println!("stop = {}", (v.stop)(d));
            println!("dispose = {}", (v.dispose_buffers)(d));
            println!("release = {}", (v.release)(d));
            CoUninitialize();
        }
    }
}
