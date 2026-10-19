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
    let out = PathBuf::from(out_dir).join("fesilent-mouse.res");
    // rc 文件引用同目录图标；从资源目录启动编译器，避免依赖调用者工作目录。
    let manifest_dir = match env::var_os("CARGO_MANIFEST_DIR") {
        Some(manifest_dir) => PathBuf::from(manifest_dir),
        None => panic!("Cargo 必须提供 CARGO_MANIFEST_DIR"),
    };
    let resource_dir = manifest_dir.join("icons/windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|target_env| target_env == "msvc");
    // 原生 Windows 构建使用 Windows SDK，无需为图标另外安装 Zig。
    // 保留 GNU 交叉编译时的 Zig 路径，继续支持 macOS → Windows 构建。
    let (mut compiler, compiler_name) = if msvc {
        let mut command = Command::new("rc.exe");
        command.args(["/nologo", "/fo"]);
        (command, "Windows SDK rc.exe")
    } else {
        let mut command = Command::new("zig");
        command.args(["rc", "/fo"]);
        (command, "zig rc")
    };
    let status = match compiler
        .current_dir(resource_dir)
        .arg(&out)
        .arg("mouseshare.rc")
        .status()
    {
        Ok(status) => status,
        Err(e) => panic!(
            "无法启动 {compiler_name} 编译 Windows 图标资源: {e}。\
             MSVC 构建请使用已安装 Windows SDK 的 Visual Studio 开发者终端。"
        ),
    };
    assert!(
        status.success(),
        "{compiler_name} 编译 Windows 图标资源失败"
    );

    println!("cargo:rustc-link-arg-bin=fesilent-mouse={}", out.display());
}
