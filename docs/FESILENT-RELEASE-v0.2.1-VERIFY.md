# v0.2.1 新图标预览版发布验证

用户于2026-10-20明确要求“更新上传”。新版本已于2026-10-20 08:44:23（Asia/Shanghai）公开：

https://github.com/id399-dzx/fesilent-mouse/releases/tag/v0.2.1

## 源码与版本

- tag：`v0.2.1`，实际提交 `766ba20b587d0cd931ab9bfb5c5fba69ddbb667a`，分支 `codex/release-0.2.1`。
- 应用源码、图标和构建脚本与本地构建源 `8d16f1060d7f96621c673ace10db7b8c4b663b9a` 完全一致。
- 发布快照沿用v0.2.0工作流（当前OAuth仍缺workflow scope）；本地宣传图目录design未随源码发布。此版本没有声称远端新版CI已通过。
- Cargo/八个本地包、Windows数值与字符串版本、Mac两个bundle版本、脚本默认版本均同步0.2.1。

## 验证

- fmt、严格Clippy、工作区255项测试通过，0失败，9项原有平台忽略。
- Mac ARM64/Windows x64 GNU release重建通过；包结构、Mac签名、安全参数启动及SHA校验通过。
- Windows EXE中七档ICO帧与新资源逐字节一致；Mac实际ICNS中的1024px图标与主PNG像素一致。
- 先上传draft，验证GitHub端三个资产状态uploaded、大小和SHA256与本地一致后公开。
- 公开后的Windows、Mac两个下载链接HTTP200；发布API确认draft=false、prerelease=true。
- v0.2.0旧发布及下载包保留。

| 资产 | 字节 | SHA256 |
|---|---:|---|
| fesilent-mouse.exe | 12661248 | 38b856843794bd73d3cab8d45941ef82525cc7f551a7468c3150ab0ccec7e84e |
| fesilent-mouse-macOS-arm64.zip | 6985948 | 5e1cc0cfadeee66e68ca3804b32b630c7613e9a52f9b6ebf427eeb38feb2977b |
| SHA256SUMS | 182 | a3ced9f5c2a031c6a17a7357d382f0c923a4ed4254b1bcaea0caceb1d5234ac1 |

产物保存在`dist/release/v0.2.1/`。发布说明保留未签名/未公证及双机真机待验证边界，不将自动检查宣称为真实Windows桌面或双机实测。
