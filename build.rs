use std::path::{Path, PathBuf};
use std::{env, fs};

const ASSETS: [&str; 2] = ["xray.exe", "sources.json"];

fn main() {
    for a in ASSETS {
        println!("cargo:rerun-if-changed=bin/{a}");
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bin_dir = manifest_dir.join("bin");

    if !bin_dir.join("xray.exe").exists() {
        println!("cargo:warning=bin/xray.exe не найден — сборка без ядра");
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let out_dir = env::var("OUT_DIR").unwrap_or_default();

    let dest_dir = match profile_dir(&out_dir, &profile) {
        Some(d) => d,
        None => {
            println!("cargo:warning=не удалось определить каталог сборки — ассеты не скопированы");
            return;
        }
    };

    for a in ASSETS {
        let src = bin_dir.join(a);
        if !src.exists() {
            continue;
        }
        let dst = dest_dir.join(a);
        if needs_copy(&src, &dst) {
            if let Err(e) = fs::copy(&src, &dst) {
                println!("cargo:warning=не удалось скопировать {a}: {e}");
            }
        }
    }
}

fn needs_copy(src: &Path, dst: &Path) -> bool {
    match (fs::metadata(src), fs::metadata(dst)) {
        (Ok(s), Ok(d)) => {
            if s.len() != d.len() {
                return true;
            }
            match (s.modified(), d.modified()) {
                (Ok(sm), Ok(dm)) => sm > dm,
                _ => true,
            }
        }
        (Ok(_), Err(_)) => true,
        _ => false,
    }
}

/// Ищет каталог профиля (target/<profile> или target/<triple>/<profile>)
/// среди предков OUT_DIR, проверяя что выше есть каталог `target`.
fn profile_dir(out_dir: &str, profile: &str) -> Option<PathBuf> {
    if out_dir.is_empty() {
        return None;
    }
    let out = PathBuf::from(out_dir);
    let ancestors: Vec<&Path> = out.ancestors().collect();
    for (i, a) in ancestors.iter().enumerate() {
        let is_profile = a
            .file_name()
            .map(|n| n == profile)
            .unwrap_or(false);
        if !is_profile {
            continue;
        }
        let under_target = ancestors[i + 1..].iter().any(|p| {
            p.file_name()
                .map(|n| n == "target")
                .unwrap_or(false)
        });
        if under_target {
            return Some(a.to_path_buf());
        }
    }
    None
}
