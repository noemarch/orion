use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE_ARCHIVE: &str = "libghostty-vt-1.0.1.tar.gz";
const DEPS_ARCHIVE: &str = "libghostty-vt-1.0.1-deps.tar.gz";
const SOURCE_DIR: &str = "libghostty-vt-1.0.1";
const DEPS_DIR: &str = "libghostty-vt-1.0.1-deps";

fn zig_target() -> String {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    let env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let zig_arch = match arch.as_str() {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        other => panic!("libghostty-vt: unsupported target arch {other}"),
    };
    let zig_os = match os.as_str() {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        "freebsd" => "freebsd",
        other => panic!("libghostty-vt: unsupported target os {other}"),
    };
    let zig_abi = if zig_os == "windows" {
        match env.as_str() {
            "msvc" => "msvc",
            "gnu" => "gnu",
            other => panic!("libghostty-vt: unsupported Windows ABI {other}"),
        }
    } else if zig_os == "macos" {
        "none"
    } else {
        "gnu"
    };
    format!("{zig_arch}-{zig_os}-{zig_abi}")
}

fn extract_archive(archive: &Path, dest: &Path, marker: &Path) {
    if marker.exists() {
        return;
    }
    fs::create_dir_all(dest).expect("create vendor extraction directory");
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .expect("failed to run `tar`; it is required to unpack vendored libghostty-vt");
    assert!(
        status.success(),
        "tar failed to extract {}",
        archive.display()
    );
    fs::write(marker, b"ok").expect("write extraction marker");
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let vendor = manifest_dir.join("vendor");
    let build_dir = out_dir.join("ghostty-build");

    let source_archive = vendor.join(SOURCE_ARCHIVE);
    let deps_archive = vendor.join(DEPS_ARCHIVE);
    for archive in [&source_archive, &deps_archive] {
        assert!(
            archive.exists(),
            "missing vendored archive: {}. Run the repository setup to fetch libghostty-vt 1.0.1.",
            archive.display()
        );
    }

    extract_archive(
        &source_archive,
        &build_dir,
        &build_dir.join(".extracted-source"),
    );
    let deps_dir = build_dir.join(".deps-extracted");
    extract_archive(&deps_archive, &deps_dir, &deps_dir.join(".extracted-deps"));

    let source = build_dir.join(SOURCE_DIR);
    let install = build_dir.join("install");
    let cache = env::var_os("ORION_ZIG_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| build_dir.join("zig-cache"));
    let system_deps = deps_dir.join(DEPS_DIR);

    let target = zig_target();
    let status = Command::new("zig")
        .current_dir(&source)
        .arg("build")
        .arg("--prefix")
        .arg(&install)
        .arg("--system")
        .arg(&system_deps)
        .arg("--cache-dir")
        .arg(&cache)
        .arg(format!("-Dtarget={target}"))
        .arg("-Doptimize=ReleaseFast")
        .arg("-Dcpu=baseline")
        .status()
        .expect("failed to run `zig`; Zig 0.17.x is required to build libghostty-vt");
    assert!(status.success(), "zig build failed for {target}");

    let lib_dir = install.join("lib");
    let (archive_name, link_name) = if target.contains("windows") {
        ("ghostty-vt-static.lib", "ghostty-vt-static")
    } else {
        ("libghostty-vt.a", "ghostty-vt")
    };
    let built = lib_dir.join(archive_name);
    assert!(
        built.exists(),
        "expected static library not found at {}",
        built.display()
    );
    let destination = out_dir.join(archive_name);
    fs::copy(&built, &destination).expect("copy libghostty-vt static library");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static={link_name}");
    println!("cargo:rerun-if-changed={}", source_archive.display());
    println!("cargo:rerun-if-changed={}", deps_archive.display());
    println!("cargo:rerun-if-env-changed=ORION_ZIG_CACHE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ENV");
}
