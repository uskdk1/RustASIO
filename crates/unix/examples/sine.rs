//! Simulates an ASIO host against the unix half: 440 Hz sine on all outputs.
//! Run: RUSTASIO_LOG=1 cargo run -p rustasio-unix --example sine [seconds]
use rustasio_abi::*;
use rustasio_unix::__wine_unix_call_funcs as F;
use std::ffi::c_void;

fn call<T>(f: Func, p: &mut T) -> i32 {
    unsafe { F[f as usize](p as *mut T as *mut c_void) }
}

fn main() {
    let secs: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let mut init = InitParams::default();
    assert_eq!(call(Func::Init, &mut init), STATUS_OK, "init");
    let c = init.config;
    println!("config: {c:?}");
    let frames = c.buffer_size as usize;
    let nout = c.n_outputs as usize;

    // [ch][half] buffers, f32 or i32
    let mut bufs: Vec<[Vec<u32>; 2]> = (0..nout).map(|_| [vec![0; frames], vec![0; frames]]).collect();
    let mut cp = CreateBuffersParams {
        frames: frames as u32,
        n_outputs: nout as u32,
        n_inputs: 0,
        sample_format: c.sample_format,
        outputs: [[0; 2]; MAX_CHANNELS],
        inputs: [[0; 2]; MAX_CHANNELS],
    };
    for (ch, b) in bufs.iter_mut().enumerate() {
        cp.outputs[ch] = [b[0].as_mut_ptr() as u64, b[1].as_mut_ptr() as u64];
    }
    assert_eq!(call(Func::CreateBuffers, &mut cp), STATUS_OK, "createBuffers");
    assert_eq!(call(Func::Start, &mut ()), STATUS_OK, "start");

    let ptrs: Vec<[u64; 2]> = cp.outputs[..nout].to_vec();
    let host = std::thread::spawn(move || {
        let mut phase = 0f64;
        let mut n = 0u64;
        let mut last_ns = 0u64;
        let mut max_gap = 0u64;
        loop {
            let mut w = WaitParams::default();
            call(Func::Wait, &mut w);
            if w.status == WAIT_STOPPED {
                break;
            }
            let rate = if w.sample_rate != 0 { w.sample_rate } else { c.sample_rate } as f64;
            let step = 440.0 * std::f64::consts::TAU / rate;
            for i in 0..frames {
                let v = (phase.sin() * 0.05) as f32;
                phase = (phase + step) % std::f64::consts::TAU;
                for p in &ptrs {
                    unsafe {
                        if c.sample_format == FMT_I32 {
                            *(p[w.half as usize] as *mut i32).add(i) = (v as f64 * 2147483647.0) as i32;
                        } else {
                            *(p[w.half as usize] as *mut f32).add(i) = v;
                        }
                    }
                }
            }
            call(Func::Done, &mut ());
            if last_ns != 0 {
                max_gap = max_gap.max(w.system_ns.saturating_sub(last_ns));
            }
            last_ns = w.system_ns;
            n += 1;
            if n % 2000 == 0 {
                println!("blocks={n} pos={} rate={} xruns={} max_gap={:.3}ms", w.sample_pos, w.sample_rate, w.xruns, max_gap as f64 / 1e6);
                max_gap = 0;
            }
        }
        println!("host thread exit after {n} blocks");
    });

    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    call(Func::Stop, &mut ());
    host.join().unwrap();
    call(Func::DisposeBuffers, &mut ());
    call(Func::Exit, &mut ());
    drop(bufs);
}
