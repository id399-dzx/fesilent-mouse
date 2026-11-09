# macOS 稳定签名与正式分发检查（F-1020）

2026-11-09。实现的是分发门禁与工具，不表示已有正式证书或已完成公证。当前本机仅可构建开发签名候选；已安装的 `/Applications/Fesilent Mouse.app`、辅助功能授权和用户配置未修改。本轮没有提交 Apple 公证、上传 GitHub 或修改网站。

## 两种构建用途

- 本机调试：沿用 `bash scripts/build-macos-app.sh 0.2.2`。自动选择唯一可用 Developer ID，其次 Apple Development；证书不唯一必须显式选择。开发签名包明确提示仅用于本机，缺证书时 ad-hoc 有 TCC 身份变化提示。
- 正式分发：显式设置 `FESILENT_MACOS_RELEASE=1`。在 cargo 前拒绝缺失证书、开发证书、自定义证书及无效模式值，只接受 Developer ID Application。签名先内部二进制再外层 App，启用 Hardened Runtime 和安全时间戳。`FESILENT_CODESIGN_TEAM_ID` 可固定团队，`FESILENT_CODESIGN_IDENTITY` 可固定40位证书hash；不要把私钥或密码写入配置或仓库。

稳定签名依靠持续使用正确团队/应用标识，不能用文件名相同保证旧TCC授权有效。公证也不能替用户授予辅助功能；系统权限始终以当前AX检查结果为准，不绕过检查。

## 分阶段工作流

以下为维护者后续明确授权正式分发时使用；本次未执行公证上传。

1. 准备有效 Developer ID Application 证书及其私钥。使用 Apple 官方 `notarytool store-credentials` 在钥匙串保存公证凭据；脚本只引用 profile 名称，不接收密码/API私钥。
2. 固定团队/证书后生成签名候选：

   ```sh
   source .tools/env.sh
   FESILENT_MACOS_RELEASE=1 bash scripts/build-macos-app.sh 0.2.2
   ```

3. 只做分发签名预检（不会上传）：

   ```sh
   python3 -B scripts/verify-macos-release.py --app dist/fesilent-mouse.app --expected-version 0.2.2
   ```

4. 维护者明确授权上传后，显式提交并装订票据：

   ```sh
   python3 -B scripts/verify-macos-release.py --app dist/fesilent-mouse.app --expected-version 0.2.2 --notarize --keychain-profile YOUR_SAVED_PROFILE
   ```

   只有 Apple 返回 `Accepted` 才会装订；结果无效、拒绝或工具失败均停止。临时提交ZIP位于项目target内，结束后清理。只校验已公证包可使用 `--require-notarized`，依次检查 stapler 与 Gatekeeper。

5. 复用已经签名/公证的App创建两个系统的包：

   ```sh
   FESILENT_MACOS_RELEASE=1 bash scripts/build-release.sh 0.2.2 --macos-app dist/fesilent-mouse.app
   ```

   此入口不重建Mac包，避免丢掉刚装订的票据；打ZIP前要求Developer ID、bundle、版本、可选固定团队、runtime、timestamp、公证票据及Gatekeeper全部通过。直接运行严格 `build-release.sh` 会先构建App；未公证时停止，留下App供步骤4使用，然后用 `--macos-app` 继续。没有隐式上传。

`0.2.2` 是本轮开发元数据，正式发布前仍须统一提升版本、确认源码与产物匹配；不能覆盖现有0.2.2公开资产。新脚本不代替发布授权。

## 验证与边界

- `python3 -B -m unittest discover -s scripts -p 'test_*.py'`：34项通过，包含开发/临时签名拒绝、固定团队、runtime/timestamp/调试权限、错误版本、公证接受/拒绝/异常、临时文件清理、无凭据时不调用工具，以及真实执行打包脚本的假工具门禁测试。
- 实机只运行严格证书选择预检：按预期因缺Developer ID停止；未输出个人证书信息。普通开发签名App本机构建、签名结构与loader参数拒绝路径通过。
- Rust293项通过、0失败、9项原有忽略；Mac/Windows严格Clippy与release构建通过。
- 缺Developer ID，无法实测真实Developer ID签名、公证服务接受、票据装订和Gatekeeper最终放行。这部分需H-3证书及后续明确分发授权，不能将mock结果当成Apple验证通过。

参考：[Apple公证要求](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)、[自定义公证工作流](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)。
