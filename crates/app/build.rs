use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    const RC: &str = "icons/windows/mouseshare.rc";
    const ICO: &str = "icons/windows/mouseshare.ico";
    println!("cargo:rerun-if-changed={RC}");
    println!("cargo:rerun-if-changed={ICO}");

    if !env::var("TARGET").is_ok_and(|target| target.contains("windows")) {
        return;
    }

    let out_dir = match env::var_os("OUT_DIR") {
        Some(out_dir) => out_dir,
        None => panic!("Cargo 必须提供 OUT_DIR"),
    };
    let out = PathBuf::from(out_dir).join("mouseshare.res");
    let status = match Command::new("zig")
        .args(["rc", "/fo"])
        .arg(&out)
        .arg(RC)
        .status()
    {
        Ok(status) => status,
        Err(e) => panic!("无法启动 `zig rc` 编译 Windows 图标资源: {e}"),
    };
    assert!(status.success(), "`zig rc` 编译 Windows 图标资源失败");

    println!("cargo:rustc-link-arg-bin=mouseshare={}", out.display());
}
