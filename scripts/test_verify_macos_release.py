"""正式分发门禁回归；全量 mock Apple 工具，不读钥匙串、不签名、不上传。"""

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("macos_release", Path(__file__).with_name("verify-macos-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

METADATA = """Executable=/private/build/fesilent-mouse.app/Contents/MacOS/fesilent-mouse
Identifier=com.fesilent.mouse
CodeDirectory v=20500 size=130 flags=0x10000(runtime) hashes=5+3 location=embedded
Authority=Developer ID Application: Example (TEAM123456)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
Timestamp=9 Nov 2026 at 12:00:00
TeamIdentifier=TEAM123456
"""


def completed(stdout=b"", stderr=b"", code=0):
    return subprocess.CompletedProcess([], code, stdout, stderr)


class ReleaseChecks(unittest.TestCase):
    def test_valid_developer_id_runtime_timestamp_and_team(self):
        self.assertEqual(release.verify_signature_metadata(METADATA, "com.fesilent.mouse", "TEAM123456"), "TEAM123456")

    def test_reject_wrong_bundle_team_authority_runtime_and_timestamp(self):
        for old, new in (
            ("Identifier=com.fesilent.mouse", "Identifier=com.example.mouse"),
            ("TeamIdentifier=TEAM123456", "TeamIdentifier=OTHER12345"),
            ("TeamIdentifier=TEAM123456", "TeamIdentifier=not set"),
            ("Authority=Developer ID Application:", "Authority=Apple Development:"),
            ("flags=0x10000(runtime)", "flags=0x0(none)"),
            ("Timestamp=9 Nov 2026 at 12:00:00", "Timestamp=none"),
            ("Timestamp=9 Nov 2026 at 12:00:00", "Signed Time=9 Nov 2026 at 12:00:00"),
        ):
            with self.subTest(field=old), self.assertRaises(release.ReleaseError):
                release.verify_signature_metadata(METADATA.replace(old, new), "com.fesilent.mouse", "TEAM123456")

    def test_unrelated_runtime_word_and_duplicate_fields_do_not_pass(self):
        output = METADATA.replace("flags=0x10000(runtime)", "flags=0x0(none)") + "Runtime Version=runtime\n"
        with self.assertRaises(release.ReleaseError):
            release.verify_signature_metadata(output, "com.fesilent.mouse")
        for duplicate in ("TeamIdentifier=TEAM123456\n", "Identifier=com.fesilent.mouse\n", "Timestamp=valid\n"):
            with self.subTest(duplicate=duplicate), self.assertRaises(release.ReleaseError):
                release.verify_signature_metadata(METADATA + duplicate, "com.fesilent.mouse")

    def test_debug_entitlements_and_unparseable_entitlements_rejected(self):
        release.verify_entitlements(b"")
        release.verify_entitlements(plistlib.dumps({"com.apple.security.get-task-allow": False}))
        for data in (plistlib.dumps({"com.apple.security.get-task-allow": True}), b"unexpected output", plistlib.dumps([])):
            with self.subTest(data=data), self.assertRaises(release.ReleaseError):
                release.verify_entitlements(data)

    def test_verify_app_checks_signature_before_reading_metadata(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="release-", dir=scratch) as directory:
            app = Path(directory) / "fesilent-mouse.app"
            (app / "Contents").mkdir(parents=True)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.fesilent.mouse"}))
            with patch.object(release, "run", side_effect=[completed(), completed(stderr=METADATA.encode()), completed()]) as run:
                self.assertEqual(release.verify_app(app, "com.fesilent.mouse", "TEAM123456"), "TEAM123456")
                commands = [call.args[0] for call in run.call_args_list]
                self.assertEqual(commands[0], ["codesign", "--verify", "--deep", "--strict", str(app)])
                self.assertEqual(len(commands), 3)
            with patch.object(release, "run", side_effect=release.ReleaseError("invalid")) as run:
                with self.assertRaises(release.ReleaseError):
                    release.verify_app(app, "com.fesilent.mouse", None)
                self.assertEqual(run.call_count, 1)

    def test_default_preflight_does_not_query_or_submit_notary(self):
        with patch.object(release.sys, "platform", "darwin"), patch.object(release, "verify_app") as verify, \
                patch.object(release, "notarize") as submit, patch.object(release, "verify_notarized") as validate, \
                redirect_stdout(io.StringIO()) as stdout:
            self.assertEqual(release.main(["--app", "dist/fesilent-mouse.app"]), 0)
            verify.assert_called_once()
            submit.assert_not_called()
            validate.assert_not_called()
            self.assertIn("尚未验证公证", stdout.getvalue())

    def test_prepared_app_version_mismatch_stops_before_any_apple_tool(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="version-", dir=scratch) as directory:
            app = Path(directory) / "fesilent-mouse.app"
            (app / "Contents").mkdir(parents=True)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": "com.fesilent.mouse",
                "CFBundleShortVersionString": "0.2.1", "CFBundleVersion": "0.2.1",
            }))
            with patch.object(release, "run") as run, self.assertRaises(release.ReleaseError):
                release.verify_app(app, "com.fesilent.mouse", None, "0.2.2")
            run.assert_not_called()

    def test_explicit_notary_requires_profile_before_any_app_tool(self):
        for arguments in (["--notarize"], ["--keychain-profile", "private-profile"]):
            with self.subTest(arguments=arguments), patch.object(release.sys, "platform", "darwin"), \
                    patch.object(release, "verify_app") as verify, patch.object(release, "notarize") as submit, \
                    redirect_stderr(io.StringIO()) as stderr:
                self.assertEqual(release.main(["--app", "dist/fesilent-mouse.app", *arguments]), 1)
                verify.assert_not_called()
                submit.assert_not_called()
                self.assertNotIn("private-profile", stderr.getvalue())

    def test_invalid_signature_prevents_notary_submission(self):
        with patch.object(release.sys, "platform", "darwin"), \
                patch.object(release, "verify_app", side_effect=release.ReleaseError("invalid")), \
                patch.object(release, "notarize") as submit, redirect_stderr(io.StringIO()):
            self.assertEqual(release.main(["--app", "dist/fesilent-mouse.app", "--notarize", "--keychain-profile", "saved-profile"]), 1)
            submit.assert_not_called()

    def test_only_accepted_notary_result_staples_and_rechecks(self):
        for response, accepted in ((b'{"status":"Accepted"}', True), (b'{"status":"Invalid"}', False),
                                   (b'{"status":"In Progress"}', False), (b"private diagnostic", False)):
            with self.subTest(response=response), patch.object(release, "run") as run, \
                    patch.object(release, "verify_notarized") as validate:
                run.side_effect = [completed(), completed(stdout=response), completed()]
                app = ROOT / "dist/fesilent-mouse.app"
                if accepted:
                    release.notarize(app, "saved-profile")
                    self.assertEqual(run.call_count, 3)
                    validate.assert_called_once_with(app)
                    self.assertEqual(run.call_args_list[-1].args[0][:3], ["xcrun", "stapler", "staple"])
                else:
                    with self.assertRaises(release.ReleaseError):
                        release.notarize(app, "saved-profile")
                    self.assertEqual(run.call_count, 2)
                    validate.assert_not_called()
                # 公证 ZIP 是临时文件；失败与成功均清理。
                submit_zip = Path(run.call_args_list[0].args[0][-1])
                self.assertFalse(submit_zip.parent.exists())

    def test_final_validation_requires_stapled_ticket_and_gatekeeper(self):
        app = ROOT / "dist/fesilent-mouse.app"
        with patch.object(release, "run") as run:
            release.verify_notarized(app)
            self.assertEqual(run.call_count, 2)
            self.assertEqual(run.call_args_list[0].args[0][:3], ["xcrun", "stapler", "validate"])
            self.assertEqual(run.call_args_list[1].args[0][:2], ["spctl", "--assess"])

    def test_tool_failure_does_not_echo_raw_diagnostics_credentials_or_commands(self):
        for failure in (completed(stdout=b"private keychain output", stderr=b"secret error", code=1), OSError("private path")):
            with self.subTest(failure=type(failure).__name__):
                kwargs = {"side_effect": failure} if isinstance(failure, OSError) else {"return_value": failure}
                with patch.object(release.subprocess, "run", **kwargs):
                    with self.assertRaises(release.ReleaseError) as error:
                        release.run(["xcrun", "--keychain-profile", "private profile"], "公证提交")
                    self.assertEqual(str(error.exception), "公证提交失败。" if not isinstance(failure, OSError) else "公证提交失败或超时。")


if __name__ == "__main__":
    unittest.main()
