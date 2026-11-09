"""签名选择回归；使用纯数据及 mock 命令，不读取真实钥匙串或执行构建/签名。"""

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("macos_signing", Path(__file__).with_name("select-macos-signing.py"))
signing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signing)

DEV_ID = "A" * 40
DEV = "B" * 40
OTHER = "C" * 40


class SigningChecks(unittest.TestCase):
    def test_parse_only_valid_numbered_hash_entries_and_deduplicate(self):
        output = f'''  1) {DEV_ID.lower()} "Developer ID Application: Example (TEAM)"
  2) {DEV_ID} "Developer ID Application: Example (TEAM)"
  3) {OTHER} "Expired Identity" (CSSMERR_TP_CERT_EXPIRED)
     1 valid identities found
unrelated output
'''
        self.assertEqual(signing.parse_identities(output), {DEV_ID: "Developer ID Application: Example (TEAM)"})

    def test_developer_id_preferred_to_development(self):
        identity, message = signing.select_identity({DEV_ID: "Developer ID Application: Example", DEV: "Apple Development: Example"})
        self.assertEqual(identity, DEV_ID)
        self.assertIn("不执行 Apple 公证", message)

    def test_unique_development_is_local_debug_with_warning(self):
        identity, message = signing.select_identity({DEV: "Apple Development: Example"})
        self.assertEqual(identity, DEV)
        self.assertIn("仅供本机调试", message)
        self.assertIn("不应作为正式分发包", message)

    def test_ambiguity_at_either_priority_requires_explicit_choice(self):
        for identities in (
            {DEV_ID: "Developer ID Application: Team 1", OTHER: "Developer ID Application: Team 2", DEV: "Apple Development: Team 1"},
            {DEV: "Apple Development: Team 1", OTHER: "Apple Development: Team 2"},
        ):
            with self.subTest(identities=identities), self.assertRaises(signing.SigningError):
                signing.select_identity(identities)

    def test_lower_priority_ambiguity_does_not_block_unique_developer_id(self):
        identity, _ = signing.select_identity({DEV_ID: "Developer ID Application: Example", DEV: "Apple Development: Team 1", OTHER: "Apple Development: Team 2"})
        self.assertEqual(identity, DEV_ID)

    def test_explicit_hash_overrides_priority_and_accepts_lowercase(self):
        identity, _ = signing.select_identity({DEV_ID: "Developer ID Application: Example", DEV: "Apple Development: Example"}, DEV.lower())
        self.assertEqual(identity, DEV)

    def test_explicit_invalid_missing_or_name_is_rejected(self):
        for explicit in ("", "-", "Apple Development: Example", "X" * 40, DEV_ID):
            with self.subTest(explicit=explicit), self.assertRaises(signing.SigningError):
                signing.select_identity({DEV: "Apple Development: Example"}, explicit)

    def test_only_zero_identities_falls_back_to_adhoc_and_warns_about_tcc(self):
        identity, message = signing.select_identity({})
        self.assertEqual(identity, "-")
        self.assertIn("辅助功能授权可能失效", message)
        with self.assertRaises(signing.SigningError):
            signing.select_identity({OTHER: "Custom Code Signing Certificate"})

    def test_explicit_other_valid_identity_remains_debug_only(self):
        identity, message = signing.select_identity({OTHER: "Custom Code Signing Certificate"}, OTHER)
        self.assertEqual(identity, OTHER)
        self.assertIn("仅供本机调试", message)

    def test_release_mode_refuses_development_custom_and_adhoc(self):
        for identities, explicit in (
            ({}, None),
            ({DEV: "Apple Development: Example"}, None),
            ({OTHER: "Custom Code Signing Certificate"}, OTHER),
            ({DEV_ID: "Developer ID Application: Example", DEV: "Apple Development: Example"}, DEV),
        ):
            with self.subTest(identities=identities, explicit=explicit), self.assertRaises(signing.SigningError):
                signing.select_identity(identities, explicit, release=True)
        identity, _ = signing.select_identity({DEV_ID: "Developer ID Application: Example"}, release=True)
        self.assertEqual(identity, DEV_ID)

    def test_team_pin_matches_complete_suffix_including_debug_mode(self):
        identities = {DEV_ID: "Developer ID Application: Example (TEAM123456)"}
        signing.select_identity(identities, release=True, expected_team="TEAM123456")
        for team in ("OTHER12345", "TEAM", "team123456", "TEAM123456\n", ""):
            with self.subTest(team=team), self.assertRaises(signing.SigningError):
                signing.select_identity(identities, expected_team=team)
        with self.assertRaises(signing.SigningError):
            signing.select_identity({}, expected_team="TEAM123456")

    def test_release_mode_environment_rejects_typos(self):
        self.assertFalse(signing.release_mode(None))
        self.assertFalse(signing.release_mode("0"))
        self.assertTrue(signing.release_mode("1"))
        for value in ("true", "", "yes", "2"):
            with self.subTest(value=value), self.assertRaises(signing.SigningError):
                signing.release_mode(value)

    def test_security_failure_does_not_silently_fall_back_or_leak_output(self):
        for result in (subprocess.CompletedProcess([], 1, "unrelated-sensitive-output", "secret diagnostic"), OSError("private path")):
            with self.subTest(result=type(result).__name__):
                stdout, stderr = io.StringIO(), io.StringIO()
                kwargs = {"side_effect": result} if isinstance(result, OSError) else {"return_value": result}
                with patch.object(signing.subprocess, "run", **kwargs), redirect_stdout(stdout), redirect_stderr(stderr):
                    self.assertEqual(signing.main(), 1)
                self.assertEqual(stdout.getvalue(), "")
                self.assertNotIn("unrelated-sensitive-output", stderr.getvalue())
                self.assertNotIn("secret diagnostic", stderr.getvalue())
                self.assertNotIn("private path", stderr.getvalue())

    def test_build_resolves_identity_before_cargo_and_does_not_leak_security_output(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="test-", dir=scratch) as directory:
            directory = Path(directory)
            security = directory / "security"
            security.write_text('#!/bin/bash\nif [ "$*" != "find-identity -v -p codesigning" ]; then exit 99; fi\nprintf \'  1) %s "Apple Development: Example"\\n\' "' + DEV + '"\nprintf \'unrelated-sensitive-output\\n\'\n', encoding="utf-8")
            security.chmod(0o755)
            cargo = directory / "cargo"
            marker = directory / "cargo-ran"
            cargo.write_text('#!/bin/bash\ntouch "$CARGO_MARKER"\nexit 23\n', encoding="utf-8")
            cargo.chmod(0o755)
            env = {**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"], "CARGO_MARKER": str(marker), "FESILENT_CODESIGN_IDENTITY": DEV_ID}
            result = subprocess.run(["bash", str(ROOT / "scripts/build-macos-app.sh")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(marker.exists(), "不可用指定身份必须在构建前失败")
            self.assertNotIn("unrelated-sensitive-output", result.stdout + result.stderr)
            env["FESILENT_CODESIGN_IDENTITY"] = DEV
            result = subprocess.run(["bash", str(ROOT / "scripts/build-macos-app.sh")], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 23)
            self.assertTrue(marker.exists(), "选择有效身份后才允许进入构建")
            self.assertIn("仅供本机调试", result.stderr)
            self.assertNotIn("unrelated-sensitive-output", result.stdout + result.stderr)
            marker.unlink()
            env["FESILENT_MACOS_RELEASE"] = "1"
            result = subprocess.run(["bash", str(ROOT / "scripts/build-macos-app.sh")], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(marker.exists(), "开发签名不能进入正式分发构建")
            self.assertIn("Developer ID Application", result.stderr)


if __name__ == "__main__":
    unittest.main()
