//! Shared ABI between the PE side (rustasio64.dll) and the unix side (rustasio64.so).
//!
//! Every parameter block is `#[repr(C)]`, contains only fixed-size fields and is
//! passed by pointer through `__wine_unix_call`. Both sides are 64-bit, so
//! pointers are carried as `u64`.
#![no_std]

pub const MAX_CHANNELS: usize = 32;

/// Function codes: index into `__wine_unix_call_funcs`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Init = 0,
    Exit,
    CreateBuffers,
    DisposeBuffers,
    Start,
    Stop,
    Wait,
    Done,
    GetPosition,
    SetRate,
    Log,
}
pub const FUNC_COUNT: usize = 11;

/// Status codes returned by unix calls (NTSTATUS-compatible; 0 = success).
pub const STATUS_OK: i32 = 0;
pub const STATUS_FAIL: i32 = 0xC000_0001u32 as i32; // STATUS_UNSUCCESSFUL
pub const STATUS_INVALID: i32 = 0xC000_000Du32 as i32; // STATUS_INVALID_PARAMETER

/// Sample formats exposed to the ASIO host.
pub const FMT_F32: u32 = 0;
pub const FMT_I32: u32 = 1;

/// Driver configuration, resolved on the unix side (config file + env).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub n_outputs: u32,
    pub n_inputs: u32,
    pub sample_format: u32,
    /// 1 = synchronous mode: the PipeWire cycle waits for the host to fill the
    /// buffer it is about to play (lowest latency, xrun-prone).
    pub sync: u32,
    pub log: u32,
    /// ring mode: frames buffered beyond one ASIO buffer (host wake-up slack)
    pub headroom: u32,
    /// PipeWire quantum forced on the graph (ring mode)
    pub quantum: u32,
    /// 1 = report only `buffer_size` to the host; 0 = let the host pick 16..4096 (power of 2)
    pub fixed_buffer: u32,
    pub _pad: u32,
}

pub const INIT_ERROR_LEN: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct InitParams {
    /// out
    pub config: Config,
    /// out, on failure: NUL-terminated UTF-8 reason
    pub error: [u8; INIT_ERROR_LEN],
}

impl Default for InitParams {
    fn default() -> Self {
        Self {
            config: Config::default(),
            error: [0; INIT_ERROR_LEN],
        }
    }
}

impl InitParams {
    /// Store `msg` (truncated on a char boundary) as the failure reason.
    pub fn set_error(&mut self, msg: &str) {
        let mut n = msg.len().min(INIT_ERROR_LEN - 1);
        while !msg.is_char_boundary(n) {
            n -= 1;
        }
        self.error[..n].copy_from_slice(&msg.as_bytes()[..n]);
        self.error[n] = 0;
    }

    pub fn error_str(&self) -> &str {
        let n = self
            .error
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(INIT_ERROR_LEN);
        core::str::from_utf8(&self.error[..n]).unwrap_or("")
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateBuffersParams {
    pub frames: u32,
    pub n_outputs: u32,
    pub n_inputs: u32,
    pub sample_format: u32,
    /// `[channel][half]` host buffers, `frames` samples each.
    pub outputs: [[u64; 2]; MAX_CHANNELS],
    pub inputs: [[u64; 2]; MAX_CHANNELS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WaitParams {
    /// out: 0 = run bufferSwitch, 1 = stopped (exit thread)
    pub status: u32,
    /// out: double buffer index the host must process now
    pub half: u32,
    /// out: sample position of this buffer
    pub sample_pos: u64,
    /// out: system time (ns, CLOCK_MONOTONIC) of this buffer
    pub system_ns: u64,
    /// out: current sample rate
    pub sample_rate: u32,
    /// out: number of xruns so far
    pub xruns: u32,
}

pub const WAIT_RUN: u32 = 0;
pub const WAIT_STOPPED: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PositionParams {
    pub sample_pos: u64,
    pub system_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SetRateParams {
    pub sample_rate: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LogParams {
    pub ptr: u64,
    pub len: u64,
}
