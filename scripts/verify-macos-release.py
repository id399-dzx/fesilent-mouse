#!/usr/bin/env python3
"""检查 Developer ID 分发包；仅显式 --notarize 才上传 Apple 公证并装订票据。

普通调用只做签名预检，不等同已公证。最终分发前使用 --require-notarized。
凭据只引用预先存入钥匙串的 notarytool profile，不接受密码/API 私钥参数。
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def run(command, description, *, timeout=30):
    # 工具原始输出可能包含钥匙串或账号诊断；错误只给阶段，不回显原文/命令参数。
    try:
        result = subprocess.run(command, check=False, capture_output=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReleaseError(f"{description}失败或超时。") from error
    require(result.returncode == 0, f"{description}失败。")
    return result


def signature_metadata(output):
    fields = {}
    for line in output.splitlines():
        if line.startswith("CodeDirectory "):
            fields.setdefault("CodeDirectory", []).append(line)
            continue
        key, separator, value = line.partition("=")
        if separator:
            fields.setdefault(key.strip(), []).append(value.strip())
    return fields


def verify_signature_metadata(output, expected_bundle, expected_team=None):
    fields = signature_metadata(output)
    require(fields.get("Identifier") == [expected_bundle], "代码签名标识与 bundle id 不一致。")
    authorities = fields.get("Authority", [])
    require(bool(authorities) and authorities[0].startswith("Developer ID Application:"),
            "正式包必须使用 Developer ID Application 签名。")
    teams = fields.get("TeamIdentifier", [])
    require(len(teams) == 1 and re.fullmatch(r"[A-Z0-9]{10}", teams[0]), "签名缺少有效 Team ID。")
    if expected_team is not None:
        require(teams == [expected_team], "签名团队与预期 Team ID 不一致。")
    # flags 位才是 runtime 的判据，避免其他输出行包含单词 runtime 造成误判。
    require(any(re.search(r"flags=0x[0-9a-fA-F]+\([^)]*\bruntime\b", value)
                for value in fields.get("CodeDirectory", [])), "正式包未启用 Hardened Runtime。")
    timestamps = fields.get("Timestamp", [])
    require(len(timestamps) == 1 and timestamps[0] not in ("", "none", "None"), "正式包缺少安全时间戳。")
    return teams[0]


def verify_entitlements(data):
    if not data.strip():
        return
    try:
        entitlements = plistlib.loads(data)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise ReleaseError("无法解析签名权限。") from error
    require(isinstance(entitlements, dict), "签名权限格式无效。")
    require(not entitlements.get("com.apple.security.get-task-allow", False),
            "正式包不能启用 get-task-allow 调试权限。")


def verify_app(app, expected_bundle, expected_team, expected_version=None):
    require(app.is_dir() and app.suffix == ".app", "请提供构建后的 .app 目录。")
    try:
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    except (OSError, plistlib.InvalidFileException, ValueError) as error:
        raise ReleaseError("无法读取 App 的 Info.plist。") from error
    require(isinstance(info, dict) and info.get("CFBundleIdentifier") == expected_bundle,
            "App 的 bundle id 与预期不一致。")
    if expected_version is not None:
        require(info.get("CFBundleShortVersionString") == expected_version
                and info.get("CFBundleVersion") == expected_version,
                "App 的版本与本次打包版本不一致。")
    run(["codesign", "--verify", "--deep", "--strict", str(app)], "完整代码签名验证")
    result = run(["codesign", "--display", "--verbose=4", str(app)], "读取代码签名")
    # codesign 的 display 元数据输出到 stderr。
    team = verify_signature_metadata(result.stderr.decode("utf-8", errors="replace"), expected_bundle, expected_team)
    result = run(["codesign", "--display", "--entitlements", ":-", str(app)], "读取签名权限")
    verify_entitlements(result.stdout)
    return team


def verify_notarized(app):
    run(["xcrun", "stapler", "validate", str(app)], "验证已装订公证票据")
    run(["spctl", "--assess", "--type", "execute", str(app)], "Gatekeeper 分发检查")


def notarize(app, profile):
    require(profile is not None and bool(profile.strip()), "--notarize 必须提供 --keychain-profile。")
    scratch = Path(__file__).resolve().parents[1] / "target/macos-notary"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="submit-", dir=scratch) as folder:
        archive = Path(folder) / "fesilent-mouse-notary.zip"
        run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)], "创建公证提交包")
        result = run(["xcrun", "notarytool", "submit", str(archive), "--keychain-profile", profile,
                      "--wait", "--output-format", "json"], "Apple 公证提交", timeout=1200)
        try:
            response = json.loads(result.stdout)
        except (ValueError, UnicodeDecodeError) as error:
            raise ReleaseError("无法解析 Apple 公证结果；未装订票据。") from error
        require(isinstance(response, dict) and response.get("status") == "Accepted",
                "Apple 未接受本次公证；未装订票据。请通过 notarytool 查询日志。")
    run(["xcrun", "stapler", "staple", str(app)], "装订 Apple 公证票据", timeout=120)
    verify_notarized(app)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--expected-bundle", default="com.fesilent.mouse")
    parser.add_argument("--expected-team", default=os.environ.get("FESILENT_CODESIGN_TEAM_ID"))
    parser.add_argument("--expected-version", help="要求 App 两个版本字段与本次打包一致")
    parser.add_argument("--require-notarized", action="store_true", help="要求已装订票据且通过 Gatekeeper")
    parser.add_argument("--notarize", action="store_true", help="显式上传 Apple、公证通过后装订并检查")
    parser.add_argument("--keychain-profile", help="已由 notarytool store-credentials 保存的钥匙串 profile")
    args = parser.parse_args(argv)
    try:
        require(sys.platform == "darwin", "分发签名与公证检查必须在 macOS 运行。")
        require(args.expected_team is None or re.fullmatch(r"[A-Z0-9]{10}", args.expected_team), "预期 Team ID 格式无效。")
        require(not args.keychain_profile or args.notarize, "--keychain-profile 仅配合显式 --notarize 使用。")
        require(not args.notarize or (args.keychain_profile and args.keychain_profile.strip()),
                "--notarize 必须提供 --keychain-profile。")
        app = args.app.resolve()
        verify_app(app, args.expected_bundle, args.expected_team, args.expected_version)
        if args.notarize:
            notarize(app, args.keychain_profile)
        elif args.require_notarized:
            verify_notarized(app)
        print("macOS 正式分发检查通过（已公证并装订）。" if args.notarize or args.require_notarized
              else "macOS 分发签名预检通过；尚未验证公证，分发前须使用 --require-notarized。")
        return 0
    except ReleaseError as error:
        print(f"macOS 分发检查失败：{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
