//! PipeWire engine: one `pw_stream` (planar F32 playback), driven by
//! the graph. The ASIO double buffer is consumed directly from host memory.
//!
//! Timing model (async, default), buffer = quantum = N frames:
//!   cycle k: play half h (filled by the host during cycle k-1), then ask the
//!   host to refill h. The host therefore has one full period to render, and the
//!   added latency is exactly one ASIO buffer.
//! Sync mode: at the start of each half, ask the host to fill it and wait (with
//! a deadline) inside the RT callback. No added buffer, but any host hiccup is
//! an audible xrun.

use crate::config::Settings;
use crate::log;
use libspa_sys as spa;
use pipewire_sys as pw;
use rustasio_abi::*;
use std::cell::UnsafeCell;
use std::ffi::{CString, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};

// ---------------------------------------------------------------------------
// futex helpers

fn futex_wait(word: &AtomicU32, expected: u32, timeout_ns: u64) {
    let ts = libc::timespec {
        tv_sec: (timeout_ns / 1_000_000_000) as _,
        tv_nsec: (timeout_ns % 1_000_000_000) as _,
    };
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            &ts as *const libc::timespec,
            ptr::null::<u32>(),
            0,
        );
    }
}

fn futex_wake(word: &AtomicU32) {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            i32::MAX,
            ptr::null::<libc::timespec>(),
            ptr::null::<u32>(),
            0,
        );
    }
}

pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

// ---------------------------------------------------------------------------
// state shared between the RT thread, the host thread and control calls

pub struct Shared {
    pub running: AtomicBool,
    /// Incremented for every fill request (and on stop). Host futex word.
    pub req_seq: AtomicU32,
    pub req_half: AtomicU32,
    pub req_pos: AtomicU64,
    pub req_ns: AtomicU64,
    /// Last request the host completed. RT futex word in sync mode.
    pub done_seq: AtomicU32,
    /// Last request handed to the host (host thread only).
    pub host_seen: AtomicU32,
    pub host_rt: AtomicBool,
    pub xruns: AtomicU32,
    pub graph_rate: AtomicU32,
    pub graph_quantum: AtomicU32,
    pub blocks: AtomicU64,
    /// output peak per channel since last report (f32 bits)
    pub peak: [AtomicU32; 2],
    // --- ring (async) mode ---
    /// frames consumed by the RT thread
    pub ring_r: AtomicU64,
    /// frames produced by the host thread
    pub ring_w: AtomicU64,
    /// bumped by the RT thread after consuming; host futex word
    pub space_seq: AtomicU32,
    /// half handed to the host by the last Wait (host thread only)
    pub host_half: AtomicU32,
}

pub static SHARED: Shared = Shared {
    running: AtomicBool::new(false),
    req_seq: AtomicU32::new(0),
    req_half: AtomicU32::new(0),
    req_pos: AtomicU64::new(0),
    req_ns: AtomicU64::new(0),
    done_seq: AtomicU32::new(0),
    host_seen: AtomicU32::new(0),
    host_rt: AtomicBool::new(false),
    xruns: AtomicU32::new(0),
    graph_rate: AtomicU32::new(0),
    graph_quantum: AtomicU32::new(0),
    blocks: AtomicU64::new(0),
    peak: [AtomicU32::new(0), AtomicU32::new(0)],
    ring_r: AtomicU64::new(0),
    ring_w: AtomicU64::new(0),
    space_seq: AtomicU32::new(0),
    host_half: AtomicU32::new(0),
};

/// SPSC ring between the host thread (writes whole ASIO buffers in `done`)
/// and the RT thread (reads `quantum` frames per cycle). Decouples host wake-up
/// jitter from the PipeWire deadline: the host may run up to `target` frames
/// ahead.
pub struct Ring {
    /// per channel, `cap` f32 samples
    data: Vec<*mut f32>,
    cap: u64,
    /// host renders while level + frames <= target
    target: u64,
    frames: u32,
    format: u32,
    /// `[channel][half]` host buffers
    host: Vec<[*const u8; 2]>,
}

impl Ring {
    fn new(nch: usize, frames: u32, headroom: u32, format: u32, host: Vec<[*const u8; 2]>) -> Self {
        let target = frames as u64 + headroom as u64;
        let cap = target + frames as u64;
        let data = (0..nch)
            .map(|_| Box::into_raw(vec![0f32; cap as usize].into_boxed_slice()) as *mut f32)
            .collect();
        Ring {
            data,
            cap,
            target,
            frames,
            format,
            host,
        }
    }

    fn clear(&self) {
        for &d in &self.data {
            unsafe { ptr::write_bytes(d, 0, self.cap as usize) };
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        for &d in &self.data {
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    d,
                    self.cap as usize,
                )));
            }
        }
    }
}

/// Current ring (null in sync mode / without buffers). Read by `wait`/`done`
/// on the host thread; freed only after the host thread is stopped.
static RING: std::sync::atomic::AtomicPtr<Ring> =
    std::sync::atomic::AtomicPtr::new(ptr::null_mut());

impl Shared {
    /// Publish a fill request for `half`. Called from the RT thread.
    #[inline]
    fn request(&self, half: u32, now_ns: u64, frames: u32) -> u32 {
        let blk = self.blocks.fetch_add(1, Relaxed);
        self.req_half.store(half, Relaxed);
        self.req_pos.store(blk * frames as u64, Relaxed);
        self.req_ns.store(now_ns, Relaxed);
        let seq = self.req_seq.fetch_add(1, Release).wrapping_add(1);
        futex_wake(&self.req_seq);
        seq
    }

    #[inline]
    fn is_done(&self, seq: u32) -> bool {
        (self.done_seq.load(Acquire).wrapping_sub(seq) as i32) >= 0
    }
}

// ---------------------------------------------------------------------------
// RT-thread-owned data

struct Rt {
    stream: *mut pw::pw_stream,
    nch: usize,
    /// SPA_IO_Position area (set by io_changed)
    position: std::sync::atomic::AtomicPtr<spa::spa_io_position>,
    /// `[channel][half]` host buffers
    bufs: Vec<[*const u8; 2]>,
    frames: u32,
    format: u32,
    sync: bool,
    sync_timeout_ns: u64,
    play_half: u32,
    cursor: u32,
    half_req: [u32; 2],
    /// async mode ring (owned by Stream)
    ring: *const Ring,
}

struct Stream {
    stream: *mut pw::pw_stream,
    hook: Box<spa::spa_hook>,
    rt: Box<UnsafeCell<Rt>>,
    ring: Option<Box<Ring>>,
}

static STREAM_EVENTS: pw::pw_stream_events = {
    let mut e: pw::pw_stream_events = unsafe { std::mem::zeroed() };
    e.version = pw::PW_VERSION_STREAM_EVENTS;
    e.state_changed = Some(on_state_changed);
    e.io_changed = Some(on_io_changed);
    e.process = Some(on_process);
    e
};

unsafe extern "C" fn on_state_changed(
    _data: *mut c_void,
    old: pw::pw_stream_state,
    state: pw::pw_stream_state,
    error: *const std::ffi::c_char,
) {
    unsafe {
        let s = |st| std::ffi::CStr::from_ptr(pw::pw_stream_state_as_string(st)).to_string_lossy();
        let err = if error.is_null() {
            String::new()
        } else {
            format!(" ({})", std::ffi::CStr::from_ptr(error).to_string_lossy())
        };
        log!("stream state {} -> {}{}", s(old), s(state), err);
    }
}

unsafe extern "C" fn on_io_changed(data: *mut c_void, id: u32, area: *mut c_void, _size: u32) {
    if id == spa::SPA_IO_Position {
        let rt = unsafe { &*(*(data as *const UnsafeCell<Rt>)).get() };
        rt.position
            .store(area as *mut spa::spa_io_position, Release);
    }
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    unsafe {
        let rt = &mut *(*(data as *const UnsafeCell<Rt>)).get();
        let b = pw::pw_stream_dequeue_buffer(rt.stream);
        if b.is_null() {
            return;
        }
        let buf = &mut *(*b).buffer;
        let nd = (buf.n_datas as usize).min(rt.nch).min(MAX_CHANNELS);

        let position = rt.position.load(Acquire);
        let (quantum, now) = if position.is_null() {
            (rt.frames, monotonic_ns())
        } else {
            let c = &(*position).clock;
            if c.rate.denom != 0 {
                SHARED.graph_rate.store(c.rate.denom, Relaxed);
            }
            (c.duration as u32, c.nsec)
        };
        let mut n = if (*b).requested != 0 {
            (*b).requested as u32
        } else {
            quantum
        };
        let mut dst: [*mut f32; MAX_CHANNELS] = [ptr::null_mut(); MAX_CHANNELS];
        for (i, d) in dst.iter_mut().enumerate().take(nd) {
            let sd = &*buf.datas.add(i);
            *d = sd.data as *mut f32;
            n = n.min(sd.maxsize / 4);
        }
        SHARED.graph_quantum.store(n, Relaxed);

        render(rt, &dst[..nd], n, now);

        for i in 0..nd {
            let sd = &mut *buf.datas.add(i);
            if !sd.chunk.is_null() {
                (*sd.chunk).offset = 0;
                (*sd.chunk).stride = 4;
                (*sd.chunk).size = n * 4;
            }
        }
        pw::pw_stream_queue_buffer(rt.stream, b);
    }
}

/// Fill `dst` (planar f32, `n` frames each) for this cycle.
unsafe fn render(rt: &mut Rt, dst: &[*mut f32], n: u32, now: u64) {
    unsafe {
        let sh = &SHARED;
        let nch = dst.len();
        if !sh.running.load(Acquire) {
            for d in dst.iter().take(nch) {
                if !d.is_null() {
                    ptr::write_bytes(*d, 0, n as usize);
                }
            }
            return;
        }

        if !rt.ring.is_null() {
            process_ring(&*rt.ring, &dst[..nch], n);
            return;
        }

        let frames = rt.frames;
        let mut off = 0u32;
        while off < n {
            let h = rt.play_half as usize;
            if rt.cursor == 0 {
                if rt.sync {
                    let seq = sh.request(h as u32, now, frames);
                    rt.half_req[h] = seq;
                    let deadline = monotonic_ns() + rt.sync_timeout_ns;
                    loop {
                        let cur = sh.done_seq.load(Acquire);
                        if (cur.wrapping_sub(seq) as i32) >= 0 {
                            break;
                        }
                        let t = monotonic_ns();
                        if t >= deadline || !sh.running.load(Relaxed) {
                            sh.xruns.fetch_add(1, Relaxed);
                            break;
                        }
                        futex_wait(&sh.done_seq, cur, deadline - t);
                    }
                } else if !sh.is_done(rt.half_req[h]) {
                    sh.xruns.fetch_add(1, Relaxed);
                }
            }

            let take = (frames - rt.cursor).min(n - off);
            for ch in 0..nch {
                let d = dst[ch];
                if d.is_null() {
                    continue;
                }
                let d = std::slice::from_raw_parts_mut(d.add(off as usize), take as usize);
                let src = rt.bufs.get(ch).map(|b| b[h]).unwrap_or(ptr::null());
                if src.is_null() {
                    d.fill(0.0);
                    continue;
                }
                match rt.format {
                    FMT_I32 => {
                        let s = std::slice::from_raw_parts(
                            (src as *const i32).add(rt.cursor as usize),
                            take as usize,
                        );
                        const SCALE: f32 = 1.0 / 2_147_483_648.0;
                        for (o, &i) in d.iter_mut().zip(s) {
                            *o = i as f32 * SCALE;
                        }
                    }
                    _ => {
                        let s = std::slice::from_raw_parts(
                            (src as *const f32).add(rt.cursor as usize),
                            take as usize,
                        );
                        d.copy_from_slice(s);
                    }
                }
                if ch < 2 {
                    // peak meter (non-negative f32 bits order like u32; NaN sorts highest)
                    let mut pk = 0f32;
                    for &v in d.iter() {
                        pk = pk.max(v.abs());
                        if v.is_nan() {
                            pk = f32::NAN;
                            break;
                        }
                    }
                    sh.peak[ch].fetch_max(pk.to_bits() & 0x7fff_ffff, Relaxed);
                }
            }
            rt.cursor += take;
            off += take;
            if rt.cursor == frames {
                rt.cursor = 0;
                if !rt.sync {
                    rt.half_req[h] = sh.request(h as u32, now, frames);
                }
                rt.play_half ^= 1;
            }
        }
    }
}

#[inline]
fn peak_of(d: &[f32]) -> u32 {
    let mut pk = 0f32;
    for &v in d {
        if v.is_nan() {
            return f32::NAN.to_bits() & 0x7fff_ffff;
        }
        pk = pk.max(v.abs());
    }
    pk.to_bits()
}

/// RT side of ring mode: copy up to `n` frames out of the ring.
unsafe fn process_ring(ring: &Ring, dst: &[*mut f32], n: u32) {
    let sh = &SHARED;
    let r = sh.ring_r.load(Relaxed);
    let w = sh.ring_w.load(Acquire);
    let avail = (w.wrapping_sub(r)).min(n as u64) as usize;
    let start = (r % ring.cap) as usize;
    let first = avail.min(ring.cap as usize - start);
    for (ch, &d) in dst.iter().enumerate() {
        if d.is_null() {
            continue;
        }
        unsafe {
            let out = std::slice::from_raw_parts_mut(d, n as usize);
            match ring.data.get(ch) {
                Some(&src) => {
                    ptr::copy_nonoverlapping(src.add(start), d, first);
                    ptr::copy_nonoverlapping(src, d.add(first), avail - first);
                    out[avail..].fill(0.0);
                }
                None => out.fill(0.0),
            }
            if ch < 2 {
                sh.peak[ch].fetch_max(peak_of(out), Relaxed);
            }
        }
    }
    if avail < n as usize {
        sh.xruns.fetch_add(1, Relaxed);
    }
    sh.ring_r.store(r + avail as u64, Release);
    sh.space_seq.fetch_add(1, Release);
    futex_wake(&sh.space_seq);
}

/// Host side of ring mode, after the host rendered `half`: append it.
fn ring_push(ring: &Ring, half: usize) {
    let sh = &SHARED;
    let w = sh.ring_w.load(Relaxed);
    let frames = ring.frames as usize;
    let start = (w % ring.cap) as usize;
    let first = frames.min(ring.cap as usize - start);
    for (ch, &dst) in ring.data.iter().enumerate() {
        let src = ring.host.get(ch).map(|b| b[half]).unwrap_or(ptr::null());
        unsafe {
            if src.is_null() {
                ptr::write_bytes(dst.add(start), 0, first);
                ptr::write_bytes(dst, 0, frames - first);
                continue;
            }
            match ring.format {
                FMT_I32 => {
                    const SCALE: f32 = 1.0 / 2_147_483_648.0;
                    let s = std::slice::from_raw_parts(src as *const i32, frames);
                    for (i, &v) in s.iter().enumerate() {
                        let pos = (start + i) % ring.cap as usize;
                        *dst.add(pos) = v as f32 * SCALE;
                    }
                }
                _ => {
                    let s = src as *const f32;
                    ptr::copy_nonoverlapping(s, dst.add(start), first);
                    ptr::copy_nonoverlapping(s.add(first), dst, frames - first);
                }
            }
        }
    }
    sh.ring_w.store(w + frames as u64, Release);
}

// ---------------------------------------------------------------------------
// control side

pub struct Engine {
    pub settings: Settings,
    tl: *mut pw::pw_thread_loop,
    ctx: *mut pw::pw_context,
    core: *mut pw::pw_core,
    stream: Option<Stream>,
}

// Raw pointers are only touched under the engine mutex / thread loop lock.
unsafe impl Send for Engine {}

pub static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

struct Lock(*mut pw::pw_thread_loop);
impl Lock {
    fn new(tl: *mut pw::pw_thread_loop) -> Self {
        unsafe { pw::pw_thread_loop_lock(tl) };
        Lock(tl)
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { pw::pw_thread_loop_unlock(self.0) };
    }
}

/// EnumFormat param: audio/raw, F32P, `rate`, `channels` with standard positions.
/// Built by hand (the SPA pod builder is header-only C).
fn audio_format_pod(rate: u32, channels: u32) -> Vec<u64> {
    const POS: [u32; 8] = [
        spa::SPA_AUDIO_CHANNEL_FL,
        spa::SPA_AUDIO_CHANNEL_FR,
        spa::SPA_AUDIO_CHANNEL_FC,
        spa::SPA_AUDIO_CHANNEL_LFE,
        spa::SPA_AUDIO_CHANNEL_RL,
        spa::SPA_AUDIO_CHANNEL_RR,
        spa::SPA_AUDIO_CHANNEL_SL,
        spa::SPA_AUDIO_CHANNEL_SR,
    ];
    let mut w: Vec<u32> = Vec::new();
    let prop_scalar = |w: &mut Vec<u32>, key: u32, ty: u32, val: u32| {
        w.extend_from_slice(&[key, 0, 4, ty, val, 0]);
    };
    // object body: type, id
    let mut body: Vec<u32> = vec![spa::SPA_TYPE_OBJECT_Format, spa::SPA_PARAM_EnumFormat];
    prop_scalar(
        &mut body,
        spa::SPA_FORMAT_mediaType,
        spa::SPA_TYPE_Id,
        spa::SPA_MEDIA_TYPE_audio,
    );
    prop_scalar(
        &mut body,
        spa::SPA_FORMAT_mediaSubtype,
        spa::SPA_TYPE_Id,
        spa::SPA_MEDIA_SUBTYPE_raw,
    );
    prop_scalar(
        &mut body,
        spa::SPA_FORMAT_AUDIO_format,
        spa::SPA_TYPE_Id,
        spa::SPA_AUDIO_FORMAT_F32P,
    );
    prop_scalar(
        &mut body,
        spa::SPA_FORMAT_AUDIO_rate,
        spa::SPA_TYPE_Int,
        rate,
    );
    prop_scalar(
        &mut body,
        spa::SPA_FORMAT_AUDIO_channels,
        spa::SPA_TYPE_Int,
        channels,
    );
    if (channels as usize) <= POS.len() {
        // array pod: header {size, Array} + child {4, Id} + values, padded to 8
        let n = channels as usize;
        let size = 8 + 4 * n as u32;
        body.extend_from_slice(&[
            spa::SPA_FORMAT_AUDIO_position,
            0,
            size,
            spa::SPA_TYPE_Array,
            4,
            spa::SPA_TYPE_Id,
        ]);
        body.extend_from_slice(&POS[..n]);
        if n % 2 == 1 {
            body.push(0);
        }
    }
    w.extend_from_slice(&[(body.len() * 4) as u32, spa::SPA_TYPE_Object]);
    w.extend_from_slice(&body);
    // u64 storage keeps 8-byte alignment
    let mut out = vec![0u64; w.len().div_ceil(2)];
    unsafe { ptr::copy_nonoverlapping(w.as_ptr(), out.as_mut_ptr() as *mut u32, w.len()) };
    out
}

fn props_set(p: *mut pw::pw_properties, k: &str, v: &str) {
    let k = CString::new(k).unwrap();
    let v = CString::new(v).unwrap();
    unsafe { pw::pw_properties_set(p, k.as_ptr(), v.as_ptr()) };
}

impl Engine {
    pub fn new(settings: Settings) -> Result<Self, String> {
        unsafe {
            pw::pw_init(ptr::null_mut(), ptr::null_mut());
            let name = CString::new("rustasio-pw").unwrap();
            let tl = pw::pw_thread_loop_new(name.as_ptr(), ptr::null());
            if tl.is_null() {
                return Err("pw_thread_loop_new failed".into());
            }
            let ctx = pw::pw_context_new(pw::pw_thread_loop_get_loop(tl), ptr::null_mut(), 0);
            if ctx.is_null() {
                pw::pw_thread_loop_destroy(tl);
                return Err("pw_context_new failed".into());
            }
            if pw::pw_thread_loop_start(tl) < 0 {
                pw::pw_context_destroy(ctx);
                pw::pw_thread_loop_destroy(tl);
                return Err("pw_thread_loop_start failed".into());
            }
            let core = {
                let _l = Lock::new(tl);
                pw::pw_context_connect(ctx, ptr::null_mut(), 0)
            };
            if core.is_null() {
                pw::pw_thread_loop_stop(tl);
                pw::pw_context_destroy(ctx);
                pw::pw_thread_loop_destroy(tl);
                return Err("cannot connect to PipeWire".into());
            }
            Ok(Engine {
                settings,
                tl,
                ctx,
                core,
                stream: None,
            })
        }
    }

    pub fn create_buffers(&mut self, p: &CreateBuffersParams) -> Result<(), String> {
        self.dispose_buffers();
        let s = &self.settings;
        let frames = p.frames;
        let nout = (p.n_outputs as usize).min(MAX_CHANNELS);
        let rate = s.sample_rate;

        unsafe {
            let props = pw::pw_properties_new(ptr::null());
            props_set(props, "media.type", "Audio");
            props_set(props, "media.category", "Playback");
            props_set(props, "media.role", "Game");
            props_set(props, "media.class", "Stream/Output/Audio");
            props_set(props, "node.name", &s.node_name);
            props_set(props, "node.description", &s.node_name);
            // ring mode: graph quantum independent of the ASIO buffer; the host
            // may render in large blocks while PipeWire pulls small ones.
            let quantum = if s.sync { frames } else { s.quantum };
            let headroom = s.headroom.max(quantum);
            props_set(props, "node.latency", &format!("{quantum}/{rate}"));
            props_set(props, "node.force-quantum", &quantum.to_string());
            props_set(props, "node.force-rate", &rate.to_string());
            props_set(props, "node.lock-quantum", "true");
            props_set(props, "node.always-process", "true");
            props_set(
                props,
                "node.autoconnect",
                if s.auto_connect { "true" } else { "false" },
            );
            if let Some(t) = &s.target {
                props_set(props, "target.object", t);
            }

            let host_bufs: Vec<[*const u8; 2]> = (0..nout)
                .map(|c| [p.outputs[c][0] as *const u8, p.outputs[c][1] as *const u8])
                .collect();
            let ring = (!s.sync).then(|| {
                Box::new(Ring::new(
                    nout,
                    frames,
                    headroom,
                    p.sample_format,
                    host_bufs.clone(),
                ))
            });
            let mut rt = Box::new(UnsafeCell::new(Rt {
                stream: ptr::null_mut(),
                nch: nout,
                position: std::sync::atomic::AtomicPtr::new(ptr::null_mut()),
                ring: ring.as_deref().map_or(ptr::null(), |r| r as *const Ring),
                bufs: host_bufs,
                frames,
                format: p.sample_format,
                sync: s.sync,
                sync_timeout_ns: frames as u64 * 1_000_000_000 / rate as u64 * 3 / 4,
                play_half: 0,
                cursor: 0,
                half_req: [0; 2],
            }));

            let _l = Lock::new(self.tl);
            let name = CString::new(s.node_name.as_str()).unwrap();
            let stream = pw::pw_stream_new(self.core, name.as_ptr(), props);
            if stream.is_null() {
                return Err("pw_stream_new failed".into());
            }
            rt.get_mut().stream = stream;
            let mut hook: Box<spa::spa_hook> = Box::new(std::mem::zeroed());
            pw::pw_stream_add_listener(
                stream,
                &mut *hook as *mut spa::spa_hook as *mut _,
                &STREAM_EVENTS,
                rt.get() as *mut c_void,
            );

            let pod = audio_format_pod(rate, nout as u32);
            let mut params = [pod.as_ptr() as *const spa::spa_pod];
            let r = pw::pw_stream_connect(
                stream,
                1, // SPA_DIRECTION_OUTPUT
                pw::PW_ID_ANY,
                pw::pw_stream_flags_PW_STREAM_FLAG_AUTOCONNECT
                    | pw::pw_stream_flags_PW_STREAM_FLAG_MAP_BUFFERS
                    | pw::pw_stream_flags_PW_STREAM_FLAG_RT_PROCESS
                    | pw::pw_stream_flags_PW_STREAM_FLAG_INACTIVE,
                params.as_mut_ptr(),
                1,
            );
            if r < 0 {
                pw::pw_stream_destroy(stream);
                return Err(format!("pw_stream_connect failed: {r}"));
            }
            log!(
                "stream created: {nout} out, {frames} frames @ {rate} Hz, {}",
                if s.sync {
                    "sync".to_string()
                } else {
                    format!("ring, quantum {quantum}, headroom {headroom}")
                }
            );
            if let Some(r) = &ring {
                RING.store(&**r as *const Ring as *mut Ring, Release);
            }
            self.stream = Some(Stream {
                stream,
                hook,
                rt,
                ring,
            });
        }
        Ok(())
    }

    pub fn dispose_buffers(&mut self) {
        self.stop();
        if let Some(st) = self.stream.take() {
            unsafe {
                let _l = Lock::new(self.tl);
                pw::pw_stream_destroy(st.stream);
            }
            RING.store(ptr::null_mut(), Release);
            drop(st.hook);
            drop(st.rt);
            drop(st.ring);
        }
    }

    pub fn start(&mut self) -> Result<(), String> {
        let Some(st) = &self.stream else {
            return Err("start without buffers".into());
        };
        let sh = &SHARED;
        unsafe {
            let _l = Lock::new(self.tl);
            // Reset RT state; the RT thread is idle (inactive stream).
            let rt = &mut *st.rt.get();
            rt.play_half = 0;
            rt.cursor = 0;
            let base = sh.req_seq.load(Acquire);
            rt.half_req = [base, base];
            sh.done_seq.store(base, Release);
            sh.host_seen.store(base, Release);
            sh.blocks.store(0, Relaxed);
            sh.xruns.store(0, Relaxed);
            sh.host_rt.store(false, Relaxed);
            if let Some(ring) = &st.ring {
                // Pre-roll silence so the host gets exactly one buffer of slack
                // beyond what the RT thread needs right away.
                ring.clear();
                sh.ring_r.store(0, Relaxed);
                sh.ring_w.store(ring.target - ring.frames as u64, Relaxed);
                sh.host_half.store(0, Relaxed);
            }
            sh.running.store(true, Release);
            pw::pw_stream_set_active(st.stream, true);
        }
        log!("started");
        Ok(())
    }

    pub fn stop(&mut self) {
        let sh = &SHARED;
        if !sh.running.swap(false, AcqRel) {
            return;
        }
        sh.req_seq.fetch_add(1, Release);
        futex_wake(&sh.req_seq);
        futex_wake(&sh.done_seq);
        sh.space_seq.fetch_add(1, Release);
        futex_wake(&sh.space_seq);
        if let Some(st) = &self.stream {
            unsafe {
                let _l = Lock::new(self.tl);
                pw::pw_stream_set_active(st.stream, false);
            }
        }
        log!("stopped (xruns: {})", sh.xruns.load(Relaxed));
    }

    pub fn set_rate(&mut self, rate: u32) {
        self.settings.sample_rate = rate;
        if let Some(st) = &self.stream {
            unsafe {
                let _l = Lock::new(self.tl);
                let rt = &*st.rt.get();
                let props = pw::pw_properties_new(ptr::null());
                props_set(props, "node.force-rate", &rate.to_string());
                let q = if rt.sync {
                    rt.frames
                } else {
                    self.settings.quantum
                };
                props_set(props, "node.latency", &format!("{q}/{rate}"));
                pw::pw_stream_update_properties(st.stream, &(*props).dict);
                pw::pw_properties_free(props);
            }
        }
    }

    /// Make the calling (host bufferSwitch) thread SCHED_FIFO, clamped to
    /// RLIMIT_RTPRIO; falls back to a negative nice value.
    pub fn make_current_thread_rt(&self) {
        let mut prio = self.settings.rt_priority;
        if prio <= 0 {
            return;
        }
        unsafe {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_RTPRIO, &mut lim) == 0
                && lim.rlim_cur != libc::RLIM_INFINITY
                && (lim.rlim_cur as i32) < prio
                && lim.rlim_cur > 0
            {
                prio = lim.rlim_cur as i32;
            }
            let param = libc::sched_param {
                sched_priority: prio,
            };
            // pid 0 = calling thread on Linux
            if libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param)
                == 0
            {
                log!("host thread: SCHED_FIFO {prio}");
                return;
            }
            let err = std::io::Error::last_os_error();
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            if libc::setpriority(libc::PRIO_PROCESS, tid, -11) == 0 {
                log!("host thread: SCHED_FIFO failed ({err}); using nice -11");
                return;
            }
            log!(
                "host thread: no realtime scheduling ({err}); raise RLIMIT_RTPRIO for better timing"
            );
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.dispose_buffers();
        unsafe {
            pw::pw_thread_loop_stop(self.tl);
            pw::pw_core_disconnect(self.core);
            pw::pw_context_destroy(self.ctx);
            pw::pw_thread_loop_destroy(self.tl);
        }
    }
}

static LOGGED_XRUNS: AtomicU32 = AtomicU32::new(0);
static LAST_REPORT_NS: AtomicU64 = AtomicU64::new(0);

/// Periodic status line (host thread, log only): output level and progress.
fn report(now: u64) {
    let last = LAST_REPORT_NS.load(Relaxed);
    if last != 0 && now.saturating_sub(last) < 2_000_000_000 {
        return;
    }
    LAST_REPORT_NS.store(now, Relaxed);
    let sh = &SHARED;
    let db = |bits: u32| {
        let v = f32::from_bits(bits);
        if v.is_nan() {
            "NaN".to_string()
        } else if v <= 0.0 {
            "-inf".to_string()
        } else {
            format!("{:.1}", 20.0 * (v as f64).log10())
        }
    };
    let l = sh.peak[0].swap(0, Relaxed);
    let r = sh.peak[1].swap(0, Relaxed);
    log!(
        "status: blocks={} peak L={} dBFS R={} dBFS xruns={} quantum={} rate={} host_lag={} ring_level={}",
        sh.blocks.load(Relaxed),
        db(l),
        db(r),
        sh.xruns.load(Relaxed),
        sh.graph_quantum.load(Relaxed),
        sh.graph_rate.load(Relaxed),
        sh.req_seq
            .load(Relaxed)
            .wrapping_sub(sh.done_seq.load(Relaxed)),
        sh.ring_w
            .load(Relaxed)
            .wrapping_sub(sh.ring_r.load(Relaxed)),
    );
}

/// Block the host thread until the next fill request (or stop).
pub fn wait(out: &mut WaitParams) {
    let sh = &SHARED;
    if !sh.host_rt.swap(true, AcqRel)
        && let Some(e) = ENGINE.lock().unwrap().as_ref()
    {
        e.make_current_thread_rt();
    }
    let ring = RING.load(Acquire);
    if !ring.is_null() {
        wait_ring(unsafe { &*ring }, out);
        return;
    }
    loop {
        if !sh.running.load(Acquire) {
            out.status = WAIT_STOPPED;
            return;
        }
        let seq = sh.req_seq.load(Acquire);
        if seq != sh.host_seen.load(Relaxed) {
            sh.host_seen.store(seq, Relaxed);
            out.status = WAIT_RUN;
            out.half = sh.req_half.load(Relaxed);
            out.sample_pos = sh.req_pos.load(Relaxed);
            out.system_ns = sh.req_ns.load(Relaxed);
            out.sample_rate = sh.graph_rate.load(Relaxed);
            out.xruns = sh.xruns.load(Relaxed);
            if out.xruns != LOGGED_XRUNS.swap(out.xruns, Relaxed) && crate::log_enabled() {
                log!(
                    "xrun (total {}), graph quantum {} rate {}",
                    out.xruns,
                    sh.graph_quantum.load(Relaxed),
                    out.sample_rate
                );
            }
            if crate::log_enabled() {
                report(out.system_ns);
            }
            return;
        }
        futex_wait(&sh.req_seq, seq, 200_000_000);
    }
}

fn wait_ring(ring: &Ring, out: &mut WaitParams) {
    let sh = &SHARED;
    loop {
        if !sh.running.load(Acquire) {
            out.status = WAIT_STOPPED;
            return;
        }
        let v = sh.space_seq.load(Acquire);
        let level = sh
            .ring_w
            .load(Relaxed)
            .wrapping_sub(sh.ring_r.load(Acquire));
        if level + ring.frames as u64 <= ring.target {
            let half = sh.host_half.fetch_xor(1, Relaxed);
            let blk = sh.blocks.fetch_add(1, Relaxed);
            let now = monotonic_ns();
            sh.req_pos.store(blk * ring.frames as u64, Relaxed);
            sh.req_ns.store(now, Relaxed);
            out.status = WAIT_RUN;
            out.half = half;
            out.sample_pos = blk * ring.frames as u64;
            out.system_ns = now;
            out.sample_rate = sh.graph_rate.load(Relaxed);
            out.xruns = sh.xruns.load(Relaxed);
            if out.xruns != LOGGED_XRUNS.swap(out.xruns, Relaxed) && crate::log_enabled() {
                log!(
                    "xrun (total {}), graph quantum {} rate {}, ring level {level}/{}",
                    out.xruns,
                    sh.graph_quantum.load(Relaxed),
                    out.sample_rate,
                    ring.target
                );
            }
            if crate::log_enabled() {
                report(now);
            }
            return;
        }
        futex_wait(&sh.space_seq, v, 200_000_000);
    }
}

/// Host finished rendering the last request.
pub fn done() {
    let sh = &SHARED;
    let ring = RING.load(Acquire);
    if !ring.is_null() {
        // the half handed out by the last wait is host_half ^ 1
        let half = (sh.host_half.load(Relaxed) ^ 1) as usize;
        ring_push(unsafe { &*ring }, half);
        return;
    }
    sh.done_seq.store(sh.host_seen.load(Relaxed), Release);
    futex_wake(&sh.done_seq);
}
