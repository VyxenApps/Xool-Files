//! Compila los recursos de Windows (icono del ejecutable) con `rc.exe`.
//!
//! El icono se incrusta en el binario para que el Explorador y los accesos
//! directos lo muestren. Si no se encuentra `rc.exe`, la compilación continúa
//! sin icono incrustado.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    if let Ok(value) = env::var("RC") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Some(path);
        }
    }

    if let Ok(paths) = env::var("PATH") {
        for dir in env::split_paths(&paths) {
            let candidate = dir.join("rc.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    for base in [env::var("ProgramFiles(x86)"), env::var("ProgramFiles")]
        .into_iter()
        .flatten()
    {
        let kits = Path::new(&base)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        let Ok(entries) = std::fs::read_dir(&kits) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        versions.sort();
        versions.reverse();
        for version in versions {
            for arch in ["x64", "x86", "arm64"] {
                let candidate = version.join(arch).join("rc.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/xool-files.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe no encontrado; el ejecutable se compilará sin icono");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let resource = out_dir.join("xool-files.res");
    let mut command = Command::new(rc);
    for part in ["MAJOR", "MINOR", "PATCH"] {
        let value = env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap();
        command.arg(format!("/dVERSION_{part}={value}"));
    }
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    command.arg(format!("/dVERSION_TEXT=\"{version}\""));
    let status = command
        .arg("/nologo")
        .arg(format!("/fo{}", resource.display()))
        .arg("assets/app.rc")
        .current_dir(&manifest)
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", resource.display());
        }
        _ => println!("cargo:warning=no se pudo compilar el recurso del icono"),
    }
}
