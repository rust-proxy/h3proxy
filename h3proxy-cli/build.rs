use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_FEATURE_CRONET_BACKEND").is_err() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let os = match target_os.as_str() {
        "linux" => "linux",
        "macos" => "mac",
        "windows" => "win",
        _ => return,
    };

    let arch = match target_arch.as_str() {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        "arm" => "arm",
        _ => return,
    };

    let version = "119.0.6045.66";
    let ext = if os == "win" { "zip" } else { "tar.xz" };
    let filename = format!("cronet-v{}-{}-{}.{}", version, os, arch, ext);
    let folder_name = format!("cronet-v{}-{}-{}", version, os, arch);
    
    let url = format!(
        "https://github.com/sleeyax/cronet-binaries/releases/download/v{}/{}",
        version, filename
    );

    let archive_path = out_dir.join(&filename);
    let extract_dir = out_dir.join(&folder_name);

    if !extract_dir.exists() {
        println!("cargo:warning=Downloading cronet binaries from {}...", url);
        let status = Command::new("curl")
            .arg("-L")
            .arg("-o")
            .arg(&archive_path)
            .arg(&url)
            .status()
            .expect("Failed to execute curl to download cronet binaries");

        if !status.success() {
            panic!("Failed to download cronet binaries");
        }

        println!("cargo:warning=Extracting {}...", filename);
        if os == "win" {
            Command::new("unzip")
                .arg(&archive_path)
                .arg("-d")
                .arg(&out_dir)
                .status()
                .expect("Failed to execute unzip");
        } else {
            Command::new("tar")
                .arg("-xf")
                .arg(&archive_path)
                .arg("-C")
                .arg(&out_dir)
                .status()
                .expect("Failed to execute tar");
        }
    }

    println!("cargo:rustc-link-search=native={}", extract_dir.display());

    // Add rpath so the binary can find the shared library at runtime
    if os == "linux" || os == "mac" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", extract_dir.display());
    }

    // Tell cargo to rebuild if this script changes
    println!("cargo:rerun-if-changed=build.rs");
}
