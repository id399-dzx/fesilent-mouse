#!/usr/bin/env python3
"""校验构建包；可选 loader 自检只经过 CLI 参数拒绝路径，不启动 UI、捕获或网络。"""

import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import struct
import subprocess
import sys
import tempfile
import zipfile


class PackageError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise PackageError(message)


def verify_checksum(artifact, manifest):
    matches = []
    for line in manifest.read_text(encoding="utf-8-sig").splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r"([0-9a-fA-F]{64})\s+\*?(.+)", line)
        require(match is not None, "SHA256SUMS 包含无效行")
        if match.group(2) == artifact.name:
            matches.append(match.group(1).lower())
    require(len(matches) == 1, "SHA256SUMS 必须恰好包含一个对应文件名")
    digest = hashlib.sha256()
    with artifact.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    require(digest.hexdigest() == matches[0], "SHA256 与清单不一致")


def verify_pe(data):
    require(len(data) >= 64 and data[:2] == b"MZ", "不是 Windows PE 可执行文件")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    require(pe + 24 <= len(data) and data[pe:pe + 4] == b"PE\0\0", "PE 标头无效")
    machine, section_count = struct.unpack_from("<HH", data, pe + 4)
    optional_size, characteristics = struct.unpack_from("<HH", data, pe + 20)
    optional = pe + 24
    require(machine == 0x8664, "Windows 包必须为 x64 架构")
    require(characteristics & 0x2 and not characteristics & 0x2000, "PE 必须为应用程序而非 DLL")
    require(optional_size >= 136 and optional + optional_size <= len(data), "PE 可选标头截断")
    require(struct.unpack_from("<H", data, optional)[0] == 0x20B, "Windows 包必须为 PE32+")
    require(struct.unpack_from("<H", data, optional + 68)[0] == 2, "Windows 发布包必须使用 GUI 子系统")
    require(struct.unpack_from("<I", data, optional + 108)[0] >= 3, "PE 缺少资源数据目录")
    resource_rva, resource_size = struct.unpack_from("<II", data, optional + 128)
    require(resource_rva != 0 and resource_size >= 16, "Windows 包缺少图标资源目录")
    sections = optional + optional_size
    require(sections + section_count * 40 <= len(data), "PE 节表截断")
    resource_offset = None
    resource_end = None
    for index in range(section_count):
        section = sections + index * 40
        virtual_size, virtual_address, raw_size, raw_offset = struct.unpack_from("<IIII", data, section + 8)
        if virtual_address <= resource_rva < virtual_address + max(virtual_size, raw_size):
            resource_offset = raw_offset + resource_rva - virtual_address
            resource_end = min(len(data), raw_offset + raw_size)
            break
    require(resource_offset is not None, "PE 资源目录无法映射到节")
    require(resource_offset + 16 <= resource_end, "PE 资源目录截断")
    named, numeric = struct.unpack_from("<HH", data, resource_offset + 12)
    require(resource_offset + 16 + (named + numeric) * 8 <= resource_end, "PE 资源项截断")
    types = {
        struct.unpack_from("<I", data, resource_offset + 16 + index * 8)[0]
        for index in range(named + numeric)
    }
    require({3, 14}.issubset(types), "PE 缺少 ICON/GROUP_ICON 图标资源")


def verify_macho(data):
    require(len(data) >= 32 and data[:4] == b"\xcf\xfa\xed\xfe", "不是预期的 64 位 Mach-O")
    require(struct.unpack_from("<I", data, 4)[0] == 0x0100000C, "macOS 包必须为 Apple Silicon ARM64")
    require(struct.unpack_from("<I", data, 12)[0] == 2, "Mach-O 必须是可执行文件")


def verify_plist(data):
    info = plistlib.loads(data)
    require(isinstance(info, dict), "Info.plist 根节点必须为字典")
    require(info.get("CFBundleIdentifier") == "com.fesilent.mouse", "bundle id 不正确")
    require(info.get("CFBundleExecutable") == "fesilent-mouse", "bundle 可执行文件名称不正确")
    require(info.get("CFBundlePackageType") == "APPL", "bundle 类型不正确")
    require(bool(info.get("CFBundleShortVersionString")), "bundle 缺少版本信息")


def run_checked(command, **kwargs):
    result = subprocess.run(command, timeout=15, check=False, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
    require(result.returncode == 0, "命令失败: " + " ".join(str(item) for item in command))


def loader_smoke(binary):
    # 此参数故意保持未知：main 的解析错误分支返回 2，且在日志/自检/捕获/网络之前。
    # Windows GUI 版会把 stdout/stderr 指向 NUL，所以只断言退出码，不依赖输出。
    result = subprocess.run([str(binary.resolve()), "--fesilent-package-invalid-argument"],
                            timeout=15, check=False, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    require(result.returncode == 2, "loader 自检未按预期在参数解析阶段退出（要求 exit 2）")


def verify_bundle(bundle, smoke):
    binary = bundle / "Contents/MacOS/fesilent-mouse"
    require(binary.is_file(), "bundle 缺少主程序")
    require(os.access(binary, os.X_OK), "bundle 主程序没有执行权限")
    verify_macho(binary.read_bytes()[:32])
    verify_plist((bundle / "Contents/Info.plist").read_bytes())
    require((bundle / "Contents/Resources/icon.icns").is_file(), "bundle 缺少图标")
    require((bundle / "Contents/_CodeSignature/CodeResources").is_file(), "bundle 缺少代码签名资源")
    if sys.platform == "darwin":
        run_checked(["codesign", "--verify", "--deep", "--strict", str(bundle)])
    if smoke:
        require(sys.platform == "darwin", "macOS loader 自检须在 macOS 上运行")
        loader_smoke(binary)


def verify_macos_zip(artifact, smoke):
    prefix = "fesilent-mouse.app/Contents/"
    with zipfile.ZipFile(artifact) as archive:
        for name in archive.namelist():
            path = PurePosixPath(name)
            require(not path.is_absolute() and ".." not in path.parts, "zip 含越界路径")
        binary = archive.getinfo(prefix + "MacOS/fesilent-mouse")
        require((binary.external_attr >> 16) & 0o111, "zip 中主程序丢失执行权限")
        with archive.open(binary) as stream:
            verify_macho(stream.read(32))
        verify_plist(archive.read(prefix + "Info.plist"))
        require(prefix + "Resources/icon.icns" in archive.namelist(), "zip 缺少应用图标")
        require(prefix + "_CodeSignature/CodeResources" in archive.namelist(), "zip 缺少代码签名")
    if sys.platform == "darwin":
        # ditto 保留可执行权限与签名文件；校验解压后的实际交付物，而非仅检查源 bundle。
        scratch = Path(__file__).resolve().parents[1] / "target/package-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="macos-", dir=scratch) as folder:
            run_checked(["ditto", "-x", "-k", str(artifact.resolve()), folder])
            verify_bundle(Path(folder) / "fesilent-mouse.app", smoke)
    else:
        require(not smoke, "macOS loader 自检须在 macOS 上运行")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=["windows", "macos"], required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--checksums", type=Path)
    parser.add_argument("--smoke", action="store_true", help="本机运行参数拒绝路径，验证加载及退出；不启动捕获")
    args = parser.parse_args()
    try:
        if args.artifact.is_dir():
            require(args.kind == "macos", "目录工件仅支持 macOS bundle")
            verify_bundle(args.artifact, args.smoke)
        else:
            require(args.checksums is not None, "文件工件必须提供 --checksums")
            verify_checksum(args.artifact, args.checksums)
            if args.kind == "windows":
                verify_pe(args.artifact.read_bytes())
                if args.smoke:
                    require(sys.platform == "win32", "Windows loader 自检须在 Windows 上运行")
                    loader_smoke(args.artifact)
            else:
                verify_macos_zip(args.artifact, args.smoke)
        print("包验证通过: " + str(args.artifact))
        return 0
    except (PackageError, OSError, ValueError, KeyError, struct.error, zipfile.BadZipFile,
            plistlib.InvalidFileException, subprocess.TimeoutExpired) as error:
        print("包验证失败: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
