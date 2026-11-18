//! ASIO SDK types (asio.h / iasiodrv.h), 64-bit Windows layout.
//! `long` is 32-bit on Windows.
#![allow(dead_code)]

use std::ffi::c_void;

pub type AsioBool = i32;
pub type AsioError = i32;
pub type AsioSampleRate = f64;

pub const ASIO_TRUE: AsioBool = 1;
pub const ASIO_FALSE: AsioBool = 0;

pub const ASE_OK: AsioError = 0;
pub const ASE_SUCCESS: AsioError = 0x3f48_47a0;
pub const ASE_NOT_PRESENT: AsioError = -1000;
pub const ASE_HW_MALFUNCTION: AsioError = -999;
pub const ASE_INVALID_PARAMETER: AsioError = -998;
pub const ASE_INVALID_MODE: AsioError = -997;
pub const ASE_SP_NOT_ADVANCING: AsioError = -996;
pub const ASE_NO_CLOCK: AsioError = -995;
pub const ASE_NO_MEMORY: AsioError = -994;

pub const ASIOST_INT32_LSB: i32 = 18;
pub const ASIOST_FLOAT32_LSB: i32 = 19;

// asioMessage selectors
pub const K_ASIO_SELECTOR_SUPPORTED: i32 = 1;
pub const K_ASIO_ENGINE_VERSION: i32 = 2;
pub const K_ASIO_RESET_REQUEST: i32 = 3;
pub const K_ASIO_BUFFER_SIZE_CHANGE: i32 = 4;
pub const K_ASIO_RESYNC_REQUEST: i32 = 5;
pub const K_ASIO_LATENCIES_CHANGED: i32 = 6;
pub const K_ASIO_SUPPORTS_TIME_INFO: i32 = 7;
pub const K_ASIO_SUPPORTS_TIME_CODE: i32 = 8;

// future() selectors
pub const K_ASIO_CAN_TIME_INFO: i32 = 10;
pub const K_ASIO_SET_IO_FORMAT: i32 = 0x2311_1961;
pub const K_ASIO_GET_IO_FORMAT: i32 = 0x2311_1983;
pub const K_ASIO_CAN_DO_IO_FORMAT: i32 = 0x2311_2004;
pub const K_ASIO_PCM_FORMAT: i32 = 0;

// AsioTimeInfo flags
pub const K_SYSTEM_TIME_VALID: u32 = 1;
pub const K_SAMPLE_POSITION_VALID: u32 = 1 << 1;
pub const K_SAMPLE_RATE_VALID: u32 = 1 << 2;
pub const K_SPEED_VALID: u32 = 1 << 3;

/// 64-bit value split into hi/lo 32-bit halves (ASIOSamples / ASIOTimeStamp).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Asio64 {
    pub hi: u32,
    pub lo: u32,
}

impl From<u64> for Asio64 {
    fn from(v: u64) -> Self {
        Asio64 { hi: (v >> 32) as u32, lo: v as u32 }
    }
}

#[repr(C)]
pub struct AsioClockSource {
    pub index: i32,
    pub associated_channel: i32,
    pub associated_group: i32,
    pub is_current_source: AsioBool,
    pub name: [u8; 32],
}

#[repr(C)]
pub struct AsioChannelInfo {
    pub channel: i32,
    pub is_input: AsioBool,
    pub is_active: AsioBool,
    pub channel_group: i32,
    pub sample_type: i32,
    pub name: [u8; 32],
}

#[repr(C)]
pub struct AsioBufferInfo {
    pub is_input: AsioBool,
    pub channel_num: i32,
    pub buffers: [*mut c_void; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AsioTimeInfo {
    pub speed: f64,
    pub system_time: Asio64,
    pub sample_position: Asio64,
    pub sample_rate: AsioSampleRate,
    pub flags: u32,
    pub reserved: [u8; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioTimeCode {
    pub speed: f64,
    pub time_code_samples: Asio64,
    pub flags: u32,
    pub future: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AsioTime {
    pub reserved: [i32; 4],
    pub time_info: AsioTimeInfo,
    pub time_code: AsioTimeCode,
}

const _: () = {
    assert!(std::mem::size_of::<AsioBufferInfo>() == 24);
    assert!(std::mem::size_of::<AsioTimeInfo>() == 48);
    assert!(std::mem::size_of::<AsioTime>() == 152);
    assert!(std::mem::size_of::<AsioChannelInfo>() == 52);
    assert!(std::mem::size_of::<AsioClockSource>() == 48);
};

#[repr(C)]
pub struct AsioCallbacks {
    pub buffer_switch: Option<unsafe extern "C" fn(index: i32, direct: AsioBool)>,
    pub sample_rate_did_change: Option<unsafe extern "C" fn(rate: AsioSampleRate)>,
    pub asio_message:
        Option<unsafe extern "C" fn(selector: i32, value: i32, msg: *mut c_void, opt: *mut f64) -> i32>,
    pub buffer_switch_time_info:
        Option<unsafe extern "C" fn(params: *mut AsioTime, index: i32, direct: AsioBool) -> *mut AsioTime>,
}

/// Copy `s` into a fixed C buffer of `cap` bytes, NUL-terminated.
pub unsafe fn copy_cstr(dst: *mut u8, cap: usize, s: &str) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let n = s.len().min(cap - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
}
