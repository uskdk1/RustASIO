//! Configuration: `$XDG_CONFIG_HOME/rustasio/config.toml` (or `~/.config/...`),
//! overridden by `RUSTASIO_*` environment variables.
//!
//! The file is a flat `key = value` list (TOML subset); sections are ignored.

use rustasio_abi::{FMT_F32, FMT_I32, MAX_CHANNELS};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub outputs: u32,
    pub sample_format: u32,
    pub sync: bool,
    /// ring mode: frames buffered beyond one ASIO buffer (host wake-up slack)
    pub headroom: u32,
    /// PipeWire quantum forced on the graph in ring mode (sync mode uses the ASIO buffer)
    pub quantum: u32,
    /// report only `buffer_size` to the host instead of letting it choose
    pub fixed_buffer: bool,
    pub log: bool,
    /// PipeWire `target.object` (node name or serial) to connect to.
    pub target: Option<String>,
    pub auto_connect: bool,
    /// SCHED_FIFO priority for the host (bufferSwitch) thread; 0 disables.
    pub rt_priority: i32,
    pub node_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            buffer_size: 64,
            outputs: 2,
            sample_format: FMT_F32,
            sync: false,
            headroom: 128,
            quantum: 64,
            fixed_buffer: false,
            log: false,
            target: None,
            auto_connect: true,
            rt_priority: 80,
            node_name: "RustASIO".into(),
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    fn apply(&mut self, key: &str, val: &str) {
        let val = val.trim().trim_matches('"').trim_matches('\'');
        match key {
            "sample_rate" | "rate" => {
                if let Ok(v) = val.parse::<u32>() {
                    if (8000..=384000).contains(&v) {
                        self.sample_rate = v;
                    }
                }
            }
            "buffer_size" | "buffer" => {
                if let Ok(v) = val.parse::<u32>() {
                    if (16..=8192).contains(&v) {
                        self.buffer_size = v;
                    }
                }
            }
            "outputs" => {
                if let Ok(v) = val.parse::<u32>() {
                    self.outputs = v.clamp(1, MAX_CHANNELS as u32);
                }
            }
            "sample_format" | "format" => match val.to_ascii_lowercase().as_str() {
                "f32" | "float" | "float32" => self.sample_format = FMT_F32,
                "i32" | "int" | "int32" => self.sample_format = FMT_I32,
                _ => {}
            },
            "sync" => self.sync = parse_bool(val).unwrap_or(self.sync),
            "headroom" => {
                if let Ok(v) = val.parse::<u32>() {
                    self.headroom = v.min(16384);
                }
            }
            "quantum" => {
                if let Ok(v) = val.parse::<u32>() {
                    if (16..=8192).contains(&v) {
                        self.quantum = v;
                    }
                }
            }
            "fixed_buffer" => self.fixed_buffer = parse_bool(val).unwrap_or(self.fixed_buffer),
            "log" => self.log = parse_bool(val).unwrap_or(self.log),
            "auto_connect" => self.auto_connect = parse_bool(val).unwrap_or(self.auto_connect),
            "target" | "output_device" => {
                self.target = if val.is_empty() { None } else { Some(val.to_string()) }
            }
            "rt_priority" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.rt_priority = v.clamp(0, 99);
                }
            }
            "node_name" => {
                if !val.is_empty() {
                    self.node_name = val.to_string();
                }
            }
            _ => {}
        }
    }

    pub fn config_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("RUSTASIO_CONFIG") {
            return Some(PathBuf::from(p));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("rustasio").join("config.toml"))
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        if let Some(path) = Self::config_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text.lines() {
                    let line = line.split('#').next().unwrap_or("").trim();
                    if line.is_empty() || line.starts_with('[') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        s.apply(k.trim(), v);
                    }
                }
            }
        }
        for (env, key) in [
            ("RUSTASIO_RATE", "sample_rate"),
            ("RUSTASIO_BUFFER", "buffer_size"),
            ("RUSTASIO_OUTPUTS", "outputs"),
            ("RUSTASIO_FORMAT", "sample_format"),
            ("RUSTASIO_SYNC", "sync"),
            ("RUSTASIO_HEADROOM", "headroom"),
            ("RUSTASIO_QUANTUM", "quantum"),
            ("RUSTASIO_FIXED_BUFFER", "fixed_buffer"),
            ("RUSTASIO_LOG", "log"),
            ("RUSTASIO_TARGET", "target"),
            ("RUSTASIO_AUTOCONNECT", "auto_connect"),
            ("RUSTASIO_RT_PRIORITY", "rt_priority"),
            ("RUSTASIO_NODE_NAME", "node_name"),
        ] {
            if let Ok(v) = std::env::var(env) {
                s.apply(key, &v);
            }
        }
        s
    }
}
