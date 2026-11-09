"""在编译前选择可用的 macOS 签名身份；stdout 只输出 codesign 所需的 hash。"""

import os
import re
import subprocess
import sys


class SigningError(Exception):
    pass


def parse_identities(output):
    """只接受 security 标为有效的身份列表，忽略汇总和其他诊断文本。"""
    identities = {}
    for line in output.splitlines():
        match = re.fullmatch(r'\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"([^"]+)"\s*', line)
        if match:
            identities[match[1].upper()] = match[2]
    return identities


def release_mode(value):
    """正式分发必须显式启用；拼写错误不能静默退回开发模式。"""
    if value not in (None, "0", "1"):
        raise SigningError("FESILENT_MACOS_RELEASE 只接受 0 或 1。")
    return value == "1"


def validate_team(team):
    if team is not None and not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise SigningError("FESILENT_CODESIGN_TEAM_ID 必须是 10 位大写字母或数字。")
    return team


def select_identity(identities, explicit=None, *, release=False, expected_team=None):
    """优先正式分发证书，自动选择只在最高优先级候选唯一时进行。"""
    validate_team(expected_team)
    if explicit is not None:
        if not re.fullmatch(r"[0-9A-Fa-f]{40}", explicit):
            raise SigningError("FESILENT_CODESIGN_IDENTITY 必须是有效签名证书的 40 位十六进制 hash。")
        selected = explicit.upper()
        if selected not in identities:
            raise SigningError("指定的签名身份不可用；请检查证书及其私钥是否位于当前钥匙串。")
    else:
        selected = None
        for prefix in ("Developer ID Application:", "Apple Development:"):
            candidates = [identity for identity, name in identities.items() if name.startswith(prefix)]
            if len(candidates) > 1:
                raise SigningError(
                    f"发现多个 {prefix[:-1]} 身份；请通过 FESILENT_CODESIGN_IDENTITY 显式指定证书 hash，避免切换团队。"
                )
            if candidates:
                selected = candidates[0]
                break
        if selected is None and identities:
            raise SigningError(
                "未找到可自动选择的 Apple 签名身份；请通过 FESILENT_CODESIGN_IDENTITY 显式指定可用证书 hash。"
            )

    if release and (selected is None or not identities[selected].startswith("Developer ID Application:")):
        raise SigningError("正式分发模式要求有效的 Developer ID Application 证书；不能使用 ad-hoc 或开发签名。")
    if expected_team is not None:
        if selected is None or not identities[selected].endswith(f" ({expected_team})"):
            raise SigningError("签名证书与指定 Team ID 不一致；拒绝改变应用的签名团队。")
    if selected is None:
        return "-", "警告：未发现有效签名证书，将使用 ad-hoc 签名。更新后二进制身份会改变，辅助功能授权可能失效；正式分发应使用固定 Developer ID Application 证书。"
    if identities[selected].startswith("Developer ID Application:"):
        return selected, "已选择 Developer ID Application 签名身份；本脚本不执行 Apple 公证。"
    return selected, "警告：已选择开发签名身份，仅供本机调试；此包未经 Apple 公证，不应作为正式分发包。请持续使用同一证书以保持应用身份。"


def main():
    try:
        # -v 仅列可用身份，不导出证书或私钥，也不打印钥匙串其他内容。
        result = subprocess.run(
            ["security", "find-identity", "-v", "-p", "codesigning"],
            text=True, capture_output=True, check=False,
        )
        if result.returncode:
            raise SigningError("无法读取有效代码签名身份；请确认当前用户的钥匙串可用。")
        identity, message = select_identity(
            parse_identities(result.stdout), os.environ.get("FESILENT_CODESIGN_IDENTITY"),
            release=release_mode(os.environ.get("FESILENT_MACOS_RELEASE")),
            expected_team=os.environ.get("FESILENT_CODESIGN_TEAM_ID"),
        )
    except (SigningError, OSError) as error:
        # 不回显 security 的原始输出，避免带出与构建无关的信息。
        message = str(error) if isinstance(error, SigningError) else "无法执行 security 签名身份查询。"
        print(f"签名选择失败：{message}", file=sys.stderr)
        return 1
    print(message, file=sys.stderr)
    print(identity)
    return 0


if __name__ == "__main__":
    sys.exit(main())
