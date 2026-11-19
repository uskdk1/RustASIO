//! libpipewire loaded at runtime with `dlopen` instead of a `DT_NEEDED` entry.
//!
//! Inside the Steam Linux Runtime the default search finds the container's
//! `libpipewire-0.3.so.0`. Outside it (e.g. plain Wine on NixOS) the library
//! is often not on the loader path; a hard link-time dependency would then make
//! Wine fail to load the whole `.so` silently. Loading it here lets us try a
//! few well-known locations and report a readable error instead.
//!
//! Only types come from `pipewire-sys`; no `pw_*` symbol is referenced at link
//! time, so `--as-needed` drops libpipewire from the ELF dependencies.

use libspa_sys as spa;
use pipewire_sys as pw;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

/// Override: full path to `libpipewire-0.3.so.0` (or a directory holding it).
pub const ENV_OVERRIDE: &str = "RUSTASIO_LIBPIPEWIRE";
const SONAME: &str = "libpipewire-0.3.so.0";

macro_rules! api {
    ($($name:ident: fn($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Api {
            $($name: unsafe extern "C" fn($($ty),*) $(-> $ret)?,)*
        }

        impl Api {
            unsafe fn resolve(h: *mut c_void) -> Result<Self, String> {
                unsafe {
                    Ok(Api {
                        $($name: {
                            let p = libc::dlsym(h, concat!(stringify!($name), "\0").as_ptr().cast());
                            if p.is_null() {
                                return Err(format!("{SONAME} lacks symbol {}", stringify!($name)));
                            }
                            std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($ty),*) $(-> $ret)?>(p)
                        },)*
                    })
                }
            }
        }

        $(
            /// # Safety
            /// Same contract as the libpipewire function; [`load`] must have succeeded.
            #[inline]
            pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                unsafe { (api().$name)($($arg),*) }
            }
        )*
    };
}

api! {
    pw_init: fn(argc: *mut c_int, argv: *mut *mut *mut c_char);
    pw_thread_loop_new: fn(name: *const c_char, props: *const spa::spa_dict) -> *mut pw::pw_thread_loop;
    pw_thread_loop_destroy: fn(l: *mut pw::pw_thread_loop);
    pw_thread_loop_get_loop: fn(l: *mut pw::pw_thread_loop) -> *mut pw::pw_loop;
    pw_thread_loop_start: fn(l: *mut pw::pw_thread_loop) -> c_int;
    pw_thread_loop_stop: fn(l: *mut pw::pw_thread_loop);
    pw_thread_loop_lock: fn(l: *mut pw::pw_thread_loop);
    pw_thread_loop_unlock: fn(l: *mut pw::pw_thread_loop);
    pw_context_new: fn(main_loop: *mut pw::pw_loop, props: *mut pw::pw_properties, user_data_size: usize) -> *mut pw::pw_context;
    pw_context_destroy: fn(ctx: *mut pw::pw_context);
    pw_context_connect: fn(ctx: *mut pw::pw_context, props: *mut pw::pw_properties, user_data_size: usize) -> *mut pw::pw_core;
    pw_core_disconnect: fn(core: *mut pw::pw_core) -> c_int;
    pw_properties_set: fn(p: *mut pw::pw_properties, key: *const c_char, value: *const c_char) -> c_int;
    pw_properties_free: fn(p: *mut pw::pw_properties);
    pw_stream_new: fn(core: *mut pw::pw_core, name: *const c_char, props: *mut pw::pw_properties) -> *mut pw::pw_stream;
    pw_stream_destroy: fn(s: *mut pw::pw_stream);
    pw_stream_add_listener: fn(s: *mut pw::pw_stream, listener: *mut spa::spa_hook, events: *const pw::pw_stream_events, data: *mut c_void);
    pw_stream_connect: fn(s: *mut pw::pw_stream, direction: spa::spa_direction, target_id: u32, flags: pw::pw_stream_flags, params: *mut *const spa::spa_pod, n_params: u32) -> c_int;
    pw_stream_dequeue_buffer: fn(s: *mut pw::pw_stream) -> *mut pw::pw_buffer;
    pw_stream_queue_buffer: fn(s: *mut pw::pw_stream, b: *mut pw::pw_buffer) -> c_int;
    pw_stream_set_active: fn(s: *mut pw::pw_stream, active: bool) -> c_int;
    pw_stream_update_properties: fn(s: *mut pw::pw_stream, dict: *const spa::spa_dict) -> c_int;
    pw_stream_state_as_string: fn(state: pw::pw_stream_state) -> *const c_char;
}

/// The variadic one is declared separately; we only ever pass a NULL key.
type PropertiesNew = unsafe extern "C" fn(key: *const c_char, ...) -> *mut pw::pw_properties;

struct Lib {
    api: Api,
    properties_new: PropertiesNew,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn api() -> &'static Api {
    &LIB.get()
        .and_then(|r| r.as_ref().ok())
        .expect("libpipewire not loaded")
        .api
}

/// # Safety
/// [`load`] must have succeeded.
pub unsafe fn pw_properties_new_empty() -> *mut pw::pw_properties {
    let f = LIB.get().unwrap().as_ref().unwrap().properties_new;
    unsafe { f(std::ptr::null()) }
}

fn dlerror() -> String {
    unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            "unknown error".into()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

/// Paths tried in order: override, default loader search, common distro/Nix dirs.
fn candidates() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os(ENV_OVERRIDE) {
        let p = std::path::PathBuf::from(p);
        v.push(
            if p.is_dir() { p.join(SONAME) } else { p }
                .to_string_lossy()
                .into_owned(),
        );
    }
    v.push(SONAME.into());
    let mut dirs: Vec<String> = vec!["/run/current-system/sw/lib".into()];
    if let Some(user) = std::env::var_os("USER") {
        dirs.push(format!(
            "/etc/profiles/per-user/{}/lib",
            user.to_string_lossy()
        ));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(format!("{}/.nix-profile/lib", home.to_string_lossy()));
    }
    dirs.extend(
        [
            "/nix/var/nix/profiles/default/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
            "/lib/x86_64-linux-gnu",
            "/lib64",
        ]
        .map(String::from),
    );
    v.extend(dirs.into_iter().map(|d| format!("{d}/{SONAME}")));
    v
}

fn open() -> Result<Lib, String> {
    let mut first_err = None;
    for path in candidates() {
        // Skip absolute paths that do not exist: their dlerror is just noise.
        if path.starts_with('/') && !std::path::Path::new(&path).exists() {
            continue;
        }
        let c = CString::new(path.as_str()).map_err(|e| e.to_string())?;
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            let e = dlerror();
            crate::log!("dlopen {path}: {e}");
            first_err.get_or_insert(e);
            continue;
        }
        let res = unsafe {
            Api::resolve(h).and_then(|api| {
                let p = libc::dlsym(h, c"pw_properties_new".as_ptr());
                if p.is_null() {
                    return Err(format!("{SONAME} lacks symbol pw_properties_new"));
                }
                Ok(Lib {
                    api,
                    properties_new: std::mem::transmute::<*mut c_void, PropertiesNew>(p),
                })
            })
        };
        match res {
            Ok(lib) => {
                crate::log!("libpipewire loaded from {path}");
                return Ok(lib);
            }
            Err(e) => {
                unsafe { libc::dlclose(h) };
                first_err.get_or_insert(e);
            }
        }
    }
    Err(format!(
        "cannot load {SONAME} ({}). Install PipeWire or set {ENV_OVERRIDE}=/path/to/{SONAME}",
        first_err.unwrap_or_else(|| "not found".into())
    ))
}

/// Load libpipewire once; later calls return the cached result.
pub fn load() -> Result<(), String> {
    LIB.get_or_init(open)
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}
