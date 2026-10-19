# 构建包验证

`verify-package.py` 仅使用 Python 3 标准库；Windows 本机构建需准备 Python 3，CI 显式
安装 Python 3.12。它已经由 `build-macos-app.sh`、`build-windows.ps1`、
`build-release.sh` 及 CI 的 macOS 压缩包步骤调用，任一验证失败都会阻止正常完成打包。

在项目根目录可以单独重验：

```sh
python3 -B scripts/verify-package.py --kind windows --artifact dist/windows-x64/fesilent-mouse.exe --checksums dist/windows-x64/SHA256SUMS
python3 -B scripts/verify-package.py --kind macos --artifact dist/fesilent-mouse-macOS-arm64.zip --checksums dist/SHA256SUMS --smoke
python3 -B -m unittest discover -s scripts -p 'test_verify_package.py' -v
```

Windows 本机使用 `python`，可在第一条命令末尾加 `--smoke`。Mac 上可以静态检查
Windows 工件；不能在 Mac 上执行 Windows loader 自检。

验证包含：

- 文件与对应 `SHA256SUMS` 的 SHA256 完全匹配；同名重复清单项会失败。
- Windows PE32+、x64、应用程序、GUI 子系统和 ICON/GROUP_ICON 资源存在。
- macOS ARM64 Mach-O、bundle id/可执行文件名/版本、图标、可执行权限和代码签名资源。
- macOS 在本机使用 `codesign --verify --deep --strict`，zip 用 `ditto` 解压后再次检查。
  临时文件只写项目 `target/package-checks/` 并自动删除。
- `--smoke` 传故意未知的参数，要求 main 在参数解析阶段以退出码 2 结束，最多等待 15 秒。
  这验证实际交付程序能加载并退出；不会初始化日志、UI、钥匙串、键鼠捕获或网络。

本验证不证明签名受信任或已公证，不替代双机输入、剪贴板、睡眠唤醒和权限撤销验收。
CLI 尚无 `--version`，`--selftest` 会访问系统安全存储，因此包验证不使用它。
