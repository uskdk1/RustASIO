//! `cargo xtask dist [--debug] | verify | package | version`
//!
//! Builds both halves and lays them out the way Wine expects builtin DLLs:
//!   dist/lib/wine/x86_64-windows/rustasio64.dll  (PE, "Wine builtin DLL" marked)
//!   dist/lib/wine/x86_64-unix/rustasio64.so      (ELF, no RUNPATH)
//! `dist` and `package` fail if the result does not pass `verify`.
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
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

fn dist(release: bool) {
    let root = root();
    let profile = if release { "release" } else { "debug" };
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }

    run(Command::new("cargo").current_dir(&root).args(&args).args([
        "-p",
        "rustasio-pe",
        "--target",
        "x86_64-pc-windows-gnu",
    ]));
    run(Command::new("cargo").current_dir(&root).args(&args).args([
        "-p",
        "rustasio-unix",
        "--lib",
        "--target",
        "x86_64-unknown-linux-gnu",
    ]));

    let out = root.join("dist/lib/wine");
    let win = out.join("x86_64-windows");
    let unix = out.join("x86_64-unix");
    std::fs::create_dir_all(&win).unwrap();
    std::fs::create_dir_all(&unix).unwrap();

    let dll = win.join(format!("{NAME}.dll"));
    std::fs::copy(
        root.join(format!(
            "target/x86_64-pc-windows-gnu/{profile}/rustasio64.dll"
        )),
        &dll,
    )
    .unwrap();
    mark_builtin(&dll);

    let so = unix.join(format!("{NAME}.so"));
    let _ = std::fs::remove_file(&so);
    std::fs::copy(
        root.join(format!(
            "target/x86_64-unknown-linux-gnu/{profile}/librustasio_unix.so"
        )),
        &so,
    )
    .unwrap();
    // Resolve libpipewire inside whatever runtime Wine runs in (Steam Linux
    // Runtime container), not from the Nix store.
    run(Command::new("patchelf").arg("--remove-rpath").arg(&so));
    if release {
        run(Command::new("strip").arg("--strip-unneeded").arg(&so));
    }

    eprintln!("\ndist ready: {}", out.display());
}

fn output(cmd: &mut Command) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn {cmd:?}: {e}"));
    if !out.status.success() {
        panic!(
            "{cmd:?} failed: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Highest GLIBC_x.y symbol version referenced by `so`.
fn max_glibc(so: &Path) -> (u32, u32) {
    let text = output(Command::new("objdump").arg("-T").arg(so));
    let mut max = (0, 0);
    for v in text.split("GLIBC_").skip(1) {
        let v = v
            .split(|c: char| c != '.' && !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        let mut it = v.split('.').map(|x| x.parse::<u32>().unwrap_or(0));
        max = max.max((it.next().unwrap_or(0), it.next().unwrap_or(0)));
    }
    max
}

/// Check the dist layout the way Wine and the Steam Linux Runtime need it.
/// Returns the list of problems (empty = ok).
fn verify() -> Vec<String> {
    let out = root().join("dist/lib/wine");
    let dll = out.join(format!("x86_64-windows/{NAME}.dll"));
    let so = out.join(format!("x86_64-unix/{NAME}.so"));
    let mut errs = Vec::new();

    // --- PE
    match std::fs::read(&dll) {
        Err(e) => errs.push(format!("{}: {e}", dll.display())),
        Ok(d) => {
            if d.get(0x40..0x40 + BUILTIN_SIGNATURE.len()) != Some(BUILTIN_SIGNATURE) {
                errs.push("dll: missing \"Wine builtin DLL\" signature".into());
            }
            let p = output(
                Command::new("x86_64-w64-mingw32-objdump")
                    .arg("-p")
                    .arg(&dll),
            );
            // Wine looks up the unix half by the export directory name.
            if !p
                .lines()
                .any(|l| l.starts_with("Name ") && l.trim_end().ends_with(&format!("{NAME}.dll")))
            {
                errs.push(format!("dll: export directory name is not {NAME}.dll"));
            }
            for sym in [
                "DllGetClassObject",
                "DllCanUnloadNow",
                "DllRegisterServer",
                "DllUnregisterServer",
            ] {
                if !p.lines().any(|l| l.split_whitespace().last() == Some(sym)) {
                    errs.push(format!("dll: missing export {sym}"));
                }
            }
        }
    }

    // --- ELF
    if !so.exists() {
        errs.push(format!("{}: missing", so.display()));
        return errs;
    }
    let dynamic = output(Command::new("readelf").arg("-d").arg(&so));
    if dynamic.contains("(RUNPATH)") || dynamic.contains("(RPATH)") {
        errs.push("so: has RUNPATH/RPATH (would load libs from the Nix store)".into());
    }
    if !dynamic.contains("[libpipewire-0.3.so.0]") {
        errs.push("so: not linked against libpipewire-0.3.so.0".into());
    }
    let syms = output(Command::new("nm").args(["-D", "--defined-only"]).arg(&so));
    if !syms
        .lines()
        .any(|l| l.split_whitespace().last() == Some("__wine_unix_call_funcs"))
    {
        errs.push("so: __wine_unix_call_funcs not exported".into());
    }
    let g = max_glibc(&so);
    eprintln!(
        "{}: highest GLIBC symbol version {}.{}",
        so.display(),
        g.0,
        g.1
    );
    if g > MAX_GLIBC {
        errs.push(format!(
            "so: needs GLIBC_{}.{} > {}.{} (Steam Linux Runtime may not load it)",
            g.0, g.1, MAX_GLIBC.0, MAX_GLIBC.1
        ));
    }
    errs
}

fn verify_or_exit() {
    let errs = verify();
    if errs.is_empty() {
        eprintln!("verify: ok");
        return;
    }
    for e in &errs {
        eprintln!("verify: {e}");
    }
    std::process::exit(1);
}

/// `target/package/rustasio-<version>-x86_64-linux.tar.gz`: dist + install scripts.
fn package() {
    let root = root();
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("rustasio-{version}-x86_64-linux");
    let dir = root.join("target/package");
    std::fs::create_dir_all(&dir).unwrap();
    let tarball = dir.join(format!("{name}.tar.gz"));
    run(Command::new("tar")
        .current_dir(&root)
        .arg("-czf")
        .arg(&tarball)
        .arg(format!("--transform=s,^,{name}/,"))
        .args(["--owner=0", "--group=0", "--sort=name"])
        .args(["dist", "scripts", "config.example.toml", "README.md"]));
    println!("{}", tarball.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = !args.iter().any(|a| a == "--debug");
    match args.first().map(String::as_str) {
        Some("dist") => {
            dist(release);
            verify_or_exit();
        }
        Some("verify") => verify_or_exit(),
        Some("package") => {
            dist(true);
            verify_or_exit();
            package();
        }
        Some("version") => println!("{}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("usage: cargo xtask <dist [--debug] | verify | package | version>");
            std::process::exit(2);
        }
    }
}
