//! `cargo xtask dist [--debug]`
//!
//! Builds both halves and lays them out the way Wine expects builtin DLLs:
//!   dist/lib/wine/x86_64-windows/rustasio64.dll  (PE, "Wine builtin DLL" marked)
//!   dist/lib/wine/x86_64-unix/rustasio64.so      (ELF, no RUNPATH)
//! Must run inside `nix develop`.

use std::path::{Path, PathBuf};
use std::process::Command;

const NAME: &str = "rustasio64";
const BUILTIN_SIGNATURE: &[u8] = b"Wine builtin DLL\0";
const MAX_GLIBC: (u32, u32) = (2, 34);

fn run(cmd: &mut Command) {
    eprintln!("+ {cmd:?}");
    let st = cmd.status().expect("spawn");
    if !st.success() {
        panic!("command failed: {st}");
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn mark_builtin(path: &Path) {
    let mut d = std::fs::read(path).unwrap();
    assert_eq!(&d[..2], b"MZ", "not a PE file");
    let e_lfanew = u32::from_le_bytes(d[0x3c..0x40].try_into().unwrap()) as usize;
    assert!(
        e_lfanew >= 0x40 + BUILTIN_SIGNATURE.len(),
        "DOS stub too small for builtin signature (e_lfanew={e_lfanew:#x})"
    );
    d[0x40..0x40 + BUILTIN_SIGNATURE.len()].copy_from_slice(BUILTIN_SIGNATURE);
    std::fs::write(path, d).unwrap();
}

fn check_glibc(so: &Path) {
    let out = Command::new("objdump").arg("-T").arg(so).output().expect("objdump");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut max = (0, 0);
    for v in text.split("GLIBC_").skip(1) {
        let v = v.split(|c: char| c != '.' && !c.is_ascii_digit()).next().unwrap_or("");
        let mut it = v.split('.').map(|x| x.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().unwrap_or(0));
        let ver = (it.next().unwrap_or(0), it.next().unwrap_or(0));
        max = max.max(ver);
    }
    eprintln!("{}: highest GLIBC symbol version {}.{}", so.display(), max.0, max.1);
    if max > MAX_GLIBC {
        eprintln!("warning: requires glibc > {}.{}; may fail in the Steam Linux Runtime", MAX_GLIBC.0, MAX_GLIBC.1);
    }
}

fn dist(release: bool) {
    let root = root();
    let profile = if release { "release" } else { "debug" };
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }

    run(Command::new("cargo")
        .current_dir(&root)
        .args(&args)
        .args(["-p", "rustasio-pe", "--target", "x86_64-pc-windows-gnu"]));
    run(Command::new("cargo")
        .current_dir(&root)
        .args(&args)
        .args(["-p", "rustasio-unix", "--lib", "--target", "x86_64-unknown-linux-gnu"]));

    let out = root.join("dist/lib/wine");
    let win = out.join("x86_64-windows");
    let unix = out.join("x86_64-unix");
    std::fs::create_dir_all(&win).unwrap();
    std::fs::create_dir_all(&unix).unwrap();

    let dll = win.join(format!("{NAME}.dll"));
    std::fs::copy(
        root.join(format!("target/x86_64-pc-windows-gnu/{profile}/rustasio64.dll")),
        &dll,
    )
    .unwrap();
    mark_builtin(&dll);

    let so = unix.join(format!("{NAME}.so"));
    let _ = std::fs::remove_file(&so);
    std::fs::copy(
        root.join(format!("target/x86_64-unknown-linux-gnu/{profile}/librustasio_unix.so")),
        &so,
    )
    .unwrap();
    // Resolve libpipewire inside whatever runtime Wine runs in (Steam Linux
    // Runtime container), not from the Nix store.
    run(Command::new("patchelf").arg("--remove-rpath").arg(&so));
    if release {
        run(Command::new("strip").arg("--strip-unneeded").arg(&so));
    }
    check_glibc(&so);

    eprintln!("\ndist ready: {}", out.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dist") => dist(!args.iter().any(|a| a == "--debug")),
        _ => {
            eprintln!("usage: cargo xtask dist [--debug]");
            std::process::exit(2);
        }
    }
}
