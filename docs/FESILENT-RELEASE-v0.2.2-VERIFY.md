# v0.2.2 发布验证

用户2026-10-20明确要求将更新版本上传GitHub。预览版于北京时间2026-10-20 09:40:12公开：

https://github.com/id399-dzx/fesilent-mouse/releases/tag/v0.2.2

## 源码

- tag `v0.2.2`、发布分支 `codex/release-0.2.2` 指向 `03866966cc62fa41771cb99a05ce096bd31c95a9`。
- GitHub默认分支main已从原8cfc77a快进到本次发布源码，单实例、自动地址/端口及新版资源可在默认分支查看；随后只补充本发布记录。
- 应用代码、资源、Cargo版本及构建脚本与本地构建提交 `bd4a21d` 完全一致。准备发布说明后的本地提交为 `639c5ed`，仅文档不同。
- 发布源码沿用远程原工作流，本地新版CI尚未推送（当前OAuth无workflow scope）；本地宣传图design目录不随源码发布。没有声称远端CI全绿。

## 包与下载

- 重新验证Mac ZIP及Windows EXE的版本、包结构和SHA，Mac签名/安全参数启动检查通过。
- 先上传draft，逐项校验GitHub端三个资产的状态uploaded、大小与SHA256；一致后公开，确认draft=false、prerelease=true。
- 公开Windows与Mac下载链接均HTTP200。
- 旧v0.2.0/v0.2.1保留，Windows前台焦点/网卡枚举及双机体验仍保持待人工。
- 开发验收的277测试、严格Clippy、双平台构建和Mac实际启动/地址检查详见 `FESILENT-STARTUP-NETWORK-VERIFY.md`。

| 资产 | 字节 | SHA256 |
|---|---:|---|
| fesilent-mouse.exe | 12750848 | f62e12706b621f779e56f9af8ae5075c28b5fcb74ae7d9c06f7cc9a7147a170a |
| fesilent-mouse-macOS-arm64.zip | 7015059 | 35afcc29600346cd8e16cb3a5d1c52c882f0ded23d7bfdb287bcf6ba42797715 |
| SHA256SUMS | 182 | 40e8e9d7425e7d1d92ba6bb0a71d13ce0eb144dccfb28336ae5760f3cc5ad617 |

本地资产仍在 `dist/release/v0.2.2/`，本机已安装同版本，不需要再次覆盖安装。
