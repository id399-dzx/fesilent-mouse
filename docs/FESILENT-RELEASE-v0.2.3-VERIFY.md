# v0.2.3 发布验证（F-1025）

用户2026-11-09明确要求更新GitHub与本机。预览版于北京时间2026-11-09 11:22:52公开：

[Fesilent Mouse v0.2.3](https://github.com/id399-dzx/fesilent-mouse/releases/tag/v0.2.3)

## 源码与版本

- 应用构建源`514c065c138b6e9f3023e953b02c339fb1853642`，Cargo工作区及8个内部包、Mac两版本字段、Windows四个版本资源均为0.2.3。
- `v0.2.3`标签和`codex/release-0.2.3`发布分支对应源码提交`0d0cb5d690dc9b7564bbc217eb41481810d4385e`，以原远程main`de09d6e`为父提交。应用、资源、Cargo和构建脚本与构建源相同。
- 当前GitHub凭据无workflow scope；发布树沿用远程整个`.github`并排除本地`design`素材。与构建源差异恰为旧workflow和5个本地海报文件，其他tracked文件完全相同。原子推送发布分支、标签及main，未force push或覆盖旧发布。
- 后续默认分支提交`342eed48328c2d1bbca6906a54d03912887d80c2`仅追加本机安装记录及Windows校验工具的输出编码修复，不改应用二进制；保持已创建标签不变。本地对应源`28a6e86`。

## 验证及分发

- fmt、Mac与Windows全目标严格Clippy通过；workspace293通过、0失败、9项原有忽略。
- 最初34项脚本测试通过；WindowsCI暴露中文输出在cp1252环境异常。真实子进程先复现有效包误失败，再修复输出的不可编码字符转义，校验规则不变；有效包exit0、坏包仍exit1，新增两回归后全36项通过。MSVC应用此前已编译及loader校验成功，失败发生在成功提示输出阶段。
- Mac ARM64与Windows GNU x64 release构建、实际PE/Mach-O架构、Windows固定版本资源、包图标、SHA、签名结构检查通过。Mac loader验证仅走参数拒绝路径，不启动输入或网络。
- 公开Mac副本从去签名的干净主程序重新ad-hoc签名；逐个ZIP文件确认无本机开发证书DER残留。App及主程序无Authority/Team，严格签名校验通过。本机固定开发签名候选不随公开包分发。
- 先上传draft，校验GitHub三资产`uploaded`状态、大小及digest与本地一致后公开，`draft=false`、`prerelease=true`。从未登录公开下载地址重新下载三资产，全部HTTP200、逐字节与本地一致；下载的Mac ZIP/Windows EXE再次通过包结构及SHA检查，Mac加载自检通过。
- 旧0.2.0/0.2.1/0.2.2保留。公开Mac包未公证，Windows未作Authenticode签名；没有把分发工具就绪描述为正式签名或公证完成。

| 资产 | 字节 | SHA256 |
|---|---:|---|
| fesilent-mouse.exe | 12796928 | c97924514535e15410dad4b62a290afd4300fa225d34c71e7ce6a06f7fd7614e |
| fesilent-mouse-macOS-arm64.zip | 7021067 | d4070fd5d7f3c66420ad222954097066c850babcdc82afb23e5ee76810474163 |
| SHA256SUMS | 182 | f43b1d355555782df032fc251a1bd981ff71e6400513403590ebfdb36547c9b7 |

原始资产位于`dist/release/v0.2.3`；重新下载的副本位于`dist/downloads/v0.2.3`；机器可读验证记录在`target/release-023`。不提交含证书身份要求、私钥或用户配置的本地证据。

## 本机与体验边界

当前Mac已安装同源0.2.3固定开发签名包，旧授权要求满足、AX首检true、真实窗口版本正确并恢复原设备连接；配置原值和配对身份保留，见[FESILENT-MAC-INSTALL-v0.2.3.md](FESILENT-MAC-INSTALL-v0.2.3.md)。没有整机重启，不声称已验证重启后持久性。

已保存设备/布局方案仍是每次一个活动连接；多设备常驻与原生文件拖拽尚未实现。Windows真实前台交互、双机滚动手感等原待人工项保持原状态，不因发布而自动标通过。

## GitHub CI

初次运行`42555103950`的Mac、格式检查成功；Windows构建成功，因上述校验输出编码误失败。默认分支追加独立工具修复后的[运行42556057121](https://github.com/id399-dzx/fesilent-mouse/actions/runs/42556057121)已全部成功：格式、Mac ARM64构建/打包/测试/Clippy、Windows原生MSVC构建/打包/loader检查/workspace测试/Clippy均通过。对应应用与工具源为`342eed4`，后续发布记录仅改Markdown，不改变该运行验证的代码。
