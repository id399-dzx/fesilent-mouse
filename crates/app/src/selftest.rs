//! 平台自检（`--selftest`）：在真机上验证已实现的平台能力，打印 PASS/FAIL。
//!
//! 当前覆盖：安全存储读写往返（macOS Keychain / Windows DPAPI）。Windows 端首次
//! 真机验证(H-2)即靠此——交叉编译出的 exe 跑 `--selftest` 回报结果即可。

use ms_platform::Platform;

fn make_platform() -> impl Platform {
    #[cfg(target_os = "macos")]
    {
        ms_platform_macos::MacPlatform::new()
    }
    #[cfg(target_os = "windows")]
    {
        ms_platform_windows::WinPlatform::new()
    }
}

/// 运行自检。返回 true 表示全部通过。
pub fn run() -> bool {
    println!("MouseShare 自检 v{}", env!("CARGO_PKG_VERSION"));
    println!(
        "平台: {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("------------------------------------");

    let platform = make_platform();
    let mut ok = true;

    ok &= check(
        "安全存储 (Keychain/DPAPI) 读写往返",
        secure_store_roundtrip(&platform),
    );

    println!("------------------------------------");
    println!(
        "结果: {}",
        if ok {
            "全部 PASS ✅"
        } else {
            "存在 FAIL ❌"
        }
    );
    ok
}

fn check(name: &str, result: Result<(), String>) -> bool {
    match result {
        Ok(()) => {
            println!("[PASS] {name}");
            true
        }
        Err(e) => {
            println!("[FAIL] {name}: {e}");
            false
        }
    }
}

fn secure_store_roundtrip(platform: &impl Platform) -> Result<(), String> {
    let mut store = platform.secure_store().map_err(|e| e.to_string())?;
    let key = "selftest-key";
    let value = b"mouseshare-selftest-secret".to_vec();

    store
        .set(key, &value)
        .map_err(|e| format!("写入失败: {e}"))?;
    let got = store.get(key).map_err(|e| format!("读取失败: {e}"))?;
    if got.as_deref() != Some(value.as_slice()) {
        return Err(format!("读回值不一致: {got:?}"));
    }
    store.delete(key).map_err(|e| format!("删除失败: {e}"))?;
    let after = store.get(key).map_err(|e| format!("删除后读取失败: {e}"))?;
    if after.is_some() {
        return Err("删除后仍能读到".into());
    }
    Ok(())
}
