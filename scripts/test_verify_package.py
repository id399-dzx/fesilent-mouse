"""包校验器回归：错误架构、窗口子系统、资源缺失和散列不一致必须被拒绝。"""

import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("verify_package", Path(__file__).with_name("verify-package.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


def pe_fixture():
    data = bytearray(0x400)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HH", data, 0x84, 0x8664, 1)
    struct.pack_into("<HH", data, 0x94, 240, 2)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<H", data, optional + 68, 2)
    struct.pack_into("<I", data, optional + 108, 16)
    struct.pack_into("<II", data, optional + 128, 0x1000, 0x100)
    section = optional + 240
    data[section:section + 8] = b".rsrc\0\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x100, 0x1000, 0x100, 0x200)
    struct.pack_into("<HH", data, 0x20C, 0, 2)
    struct.pack_into("<IIII", data, 0x210, 3, 0x80000020, 14, 0x80000040)
    return data


class PackageChecks(unittest.TestCase):
    def test_windows_x64_gui_and_icon_resources(self):
        package.verify_pe(pe_fixture())

    def test_windows_wrong_architecture_or_console_subsystem(self):
        for offset, value in [(0x84, 0xAA64), (0x98 + 68, 3)]:
            data = pe_fixture()
            struct.pack_into("<H", data, offset, value)
            with self.assertRaises(package.PackageError):
                package.verify_pe(data)

    def test_windows_missing_icon_and_truncated_resource(self):
        data = pe_fixture()
        struct.pack_into("<I", data, 0x218, 4)
        with self.assertRaises(package.PackageError):
            package.verify_pe(data)
        with self.assertRaises(package.PackageError):
            package.verify_pe(pe_fixture()[:0x21A])

    def test_mac_architecture_must_be_arm64_executable(self):
        data = bytearray(32)
        struct.pack_into("<IIII", data, 0, 0xFEEDFACF, 0x0100000C, 0, 2)
        package.verify_macho(data)
        struct.pack_into("<I", data, 4, 0x01000007)
        with self.assertRaises(package.PackageError):
            package.verify_macho(data)

    def test_checksum_detects_modified_artifact_and_duplicate_entries(self):
        scratch = Path(__file__).resolve().parents[1] / "target/package-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="test-", dir=scratch) as folder:
            binary = Path(folder) / "fesilent-mouse.exe"
            manifest = Path(folder) / "SHA256SUMS"
            binary.write_bytes(b"build output")
            line = hashlib.sha256(binary.read_bytes()).hexdigest() + "  " + binary.name + "\n"
            manifest.write_text(line, encoding="ascii")
            package.verify_checksum(binary, manifest)
            binary.write_bytes(b"changed")
            with self.assertRaises(package.PackageError):
                package.verify_checksum(binary, manifest)
            binary.write_bytes(b"build output")
            manifest.write_text(line + line, encoding="ascii")
            with self.assertRaises(package.PackageError):
                package.verify_checksum(binary, manifest)


if __name__ == "__main__":
    unittest.main()
