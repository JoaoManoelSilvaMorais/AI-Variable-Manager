use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");

    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Falha ao compilar ui/app.slint com estilo Fluent");

    #[cfg(target_os = "windows")]
    compile_windows_resources();
}

#[cfg(target_os = "windows")]
fn compile_windows_resources() {
    let rc_script = Path::new("assets/app.rc");
    if !rc_script.exists() {
        return;
    }

    let Some(rc_exe) = find_windows_rc_exe() else {
        println!(
            "cargo:warning=rc.exe não localizado; executável será compilado sem recurso Win32 embutido."
        );
        return;
    };

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR ausente"));
    let res_path = out_dir.join("app.res");

    let status = Command::new(&rc_exe)
        .args([
            "/nologo",
            "/fo",
            res_path.to_str().expect("caminho inválido para app.res"),
            rc_script.to_str().expect("caminho inválido para app.rc"),
        ])
        .status();

    match status {
        Ok(s) if s.success() && res_path.exists() => {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
        }
        Ok(s) => {
            println!("cargo:warning=rc.exe encerrou com status {s}; ignorando app.res.");
        }
        Err(err) => {
            println!("cargo:warning=Falha ao executar rc.exe: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn find_windows_rc_exe() -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for kits_root in [
        Path::new(r"D:\Windows Kits\10\bin"),
        Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin"),
        Path::new(r"C:\Program Files\Windows Kits\10\bin"),
    ] {
        if let Ok(entries) = std::fs::read_dir(kits_root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join("x64").join("rc.exe"))
                .filter(|p| p.is_file())
                .collect();
            versions.sort();
            if let Some(latest) = versions.pop() {
                return Some(latest);
            }
        }
    }

    None
}
