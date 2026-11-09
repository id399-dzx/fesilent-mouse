# Mac 辅助功能授权显示不一致（2026-10-23）

## 本机事实与边界

- 用户重启后原安装0.2.2持续提示授权。系统设置中的 Fesilent Mouse 开关确为 on；应用直接实时调用 AXIsProcessTrusted，每帧重查，无应用侧权限缓存。
- 完整终止旧进程并重开原包后仍出现授权按钮。仅一个正确路径的主进程，LaunchAgent也指向 /Applications/Fesilent Mouse.app/Contents/MacOS/fesilent-mouse。
- 原安装包为ad-hoc，designated requirement仅绑定精确cdhash。此前重建/更新会改变该身份；旧TCC记录不匹配是合理怀疑，未读取受保护的TCC数据库，不能把本次具体根因断言为旧hash。
- 本机修复包新增首次/变化AX状态日志，安装后的当前进程日志明确trusted=false；没有用设置开关外观替代系统授权结果。

## 已做的修复

- 打包优先使用唯一Developer ID Application，其次唯一Apple Development；多个候选要求显式FESILENT_CODESIGN_IDENTITY，指定不可用即在构建前失败。无可用证书时保留ad-hoc并警告。
- 当前Mac已有有效Apple Development证书，本机修复包改用该证书，其身份要求基于bundle id与Apple证书，不再仅绑定二进制hash。开发签名只供本机，本次未公证或发布，对外下载仍是原版本。
- 新增授权已勾选但失效的移除/重加指引；若系统API已返回true且原会话停止在PermissionRequired，显示“辅助功能授权已就绪”及重新连接入口，隐藏该阶段的旧权限错误。仅改变展示，不自动重连，不清除后台错误，不绕过false或权限撤销安全回退。
- 已备份旧安装包到dist/install-backups/2026-10-23-permission/Fesilent Mouse.app；已替换 /Applications/Fesilent Mouse.app（仍为0.2.2本机补丁，和GitHub公开包不同）。用户配置、配对证书和已保存设备保留。

## 用户最后一步（H-1）

应用已退出。系统设置 → 隐私与安全性 → 辅助功能中，选中Fesilent Mouse，点减号移除旧记录；点加号，添加 /Applications/Fesilent Mouse.app，开启权限。若系统要求密码或Touch ID由用户本人确认。随后重新打开应用。正常应不再显示“打开系统设置并授权”，日志新增trusted=true。若仍失败，再核对实际添加路径，不重置其他应用权限。

## 验证

Mac fmt、严格Clippy、workspace测试278通过/0失败/9原有忽略；签名选择11项测试通过；Mac release构建、包验证/loader smoke、安装签名验证通过，真实窗口新增恢复提示可见。Windows release交叉编译与严格Clippy通过（初次直接cargo clippy缺mingw工具链，改用项目已有Zig包装器后通过）；release链接器保留已有warning。原始结果见target/permission-windows-build.txt与target/permission-windows-clippy.txt。真实授权重加及重启持久性仍待用户完成H-1，不能标为恢复成功。

Apple官方依据：
- https://developer.apple.com/documentation/applicationservices/1460720-axisprocesstrusted
- https://developer.apple.com/library/archive/technotes/tn2206/_index.html

## 用户重加后进一步定位（2026-10-23 15:37）

用户报告已移除并重新添加；系统设置仍显示on。重新打开当前固定签名包后，同进程日志仍trusted=false。使用/usr/bin/log读取本应用的系统TCC日志，已证实拒绝来自旧code requirement未刷新：tccd对com.fesilent.mouse的实际请求归属和Applications路径均正确，随后明确“Failed to match existing code requirement ... kTCCServiceAccessibility”，返回authValue=0/authReason=5。系统设置15:36设置Allowed后，旧精确cdhash规则仍被保留；不是只需反复关开，也不是Rust FFI虚假返回。

下一步拟只重置本bundle的记录：`tccutil reset Accessibility com.fesilent.mouse`，不重置其他应用，不改受保护数据库。已通过对话请求用户明确授权执行此命令（项目H-1原约定由用户执行）；尚未获答，未执行。重置前退出应用与系统设置，重置后打开当前应用并由用户开启辅助功能，再以trusted=true和真实UI确认恢复。尚不能标记问题解决。

## 已获授权执行定向重置

用户明确允许“只重置Fesilent Mouse”。已退出应用与系统设置，执行 `/usr/bin/tccutil reset Accessibility com.fesilent.mouse` 成功（exit0）；命令仅指定本bundle和Accessibility服务。随后重开当前包并用应用内按钮生成新的权限请求，系统设置中当前Fesilent Mouse开关为off，其他应用开关保持原样。已请求用户亲自开启该开关，无需再移除/添加；实际trusted=true验证仍待完成。

## 恢复确认（2026-10-23 15:43）

用户在定向重置后亲自开启辅助功能。当前应用日志07:42:49Z由trusted=false变为trusted=true，真实窗口授权卡片消失，恢复“尚未连接设备 / 等待对端接入”。随后完整终止并重开应用，新进程日志07:43:15Z首次检查即trusted=true，确认本次重开保持授权。原问题已在当前Mac恢复；没有重启整台电脑或执行双机互控，整机重启持久性仍作为后续回归边界。固定开发签名本机补丁未发布，对外包仍是原0.2.2。
