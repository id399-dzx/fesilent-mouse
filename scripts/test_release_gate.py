"""真实执行打包脚本，用临时项目和替身工具验证公证门禁不被 ZIP/构建绕过。"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ReleaseGateChecks(unittest.TestCase):
    def fixture(self, folder, accepted):
        root = Path(folder)
        (root / "scripts").mkdir()
        shutil.copyfile(ROOT / "scripts/build-release.sh", root / "scripts/build-release.sh")
        (root / "scripts/build-macos-app.sh").write_text(
            '#!/bin/bash\necho build >> calls\nmkdir -p dist/fesilent-mouse.app\n', encoding="utf-8")
        commands = root / "tools"
        commands.mkdir()
        for tool, body in {
            "python3": 'echo "python $*" >> calls\n'
                       'if [[ "$*" == *--require-notarized* && "$GATE_ACCEPTED" != 1 ]]; then exit 1; fi\n',
            "ditto": 'echo archive >> calls\ntouch "${@: -1}"\n',
            "cargo": 'echo windows-build >> calls\nmkdir -p target/x86_64-pc-windows-gnu/release\n'
                     'touch target/x86_64-pc-windows-gnu/release/fesilent-mouse.exe\n',
        }.items():
            path = commands / tool
            path.write_text("#!/bin/bash\n" + body, encoding="utf-8")
            path.chmod(0o755)
        app = root / "prepared/fesilent-mouse.app"
        app.mkdir(parents=True)
        marker = app / "stapled-marker"
        marker.write_text("unchanged", encoding="ascii")
        env = os.environ.copy()
        env.update(PATH=str(commands) + os.pathsep + env["PATH"],
                   FESILENT_MACOS_RELEASE="1", GATE_ACCEPTED="1" if accepted else "0")
        return root, app, marker, env

    def run_packaging(self, root, env, *extra):
        return subprocess.run(["/bin/bash", "scripts/build-release.sh", "0.2.2", *extra],
                              cwd=root, env=env, capture_output=True, text=True, timeout=10)

    def test_prepared_unnotarized_app_cannot_be_archived_or_trigger_windows_build(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="gate-", dir=scratch) as folder:
            root, app, marker, env = self.fixture(folder, False)
            result = self.run_packaging(root, env, "--macos-app", str(app))
            self.assertEqual(result.returncode, 1)
            calls = (root / "calls").read_text()
            self.assertIn("--expected-version 0.2.2 --require-notarized", calls)
            self.assertNotIn("archive", calls)
            self.assertNotIn("windows-build", calls)
            self.assertNotIn("\nbuild\n", "\n" + calls)
            self.assertEqual(marker.read_text(), "unchanged")

    def test_prepared_valid_app_is_checked_before_archive_and_never_rebuilt(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="gate-", dir=scratch) as folder:
            root, app, marker, env = self.fixture(folder, True)
            result = self.run_packaging(root, env, "--macos-app", str(app))
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = (root / "calls").read_text()
            self.assertLess(calls.index("--require-notarized"), calls.index("archive"))
            self.assertNotIn("\nbuild\n", "\n" + calls)
            self.assertNotIn("--notarize", calls)
            self.assertEqual(marker.read_text(), "unchanged")

    def test_fresh_strict_build_leaves_app_and_stops_before_zip_until_notarized(self):
        scratch = ROOT / "target/signing-checks"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="gate-", dir=scratch) as folder:
            root, _, _, env = self.fixture(folder, False)
            result = self.run_packaging(root, env)
            self.assertEqual(result.returncode, 1)
            calls = (root / "calls").read_text()
            self.assertTrue(calls.startswith("build\n"))
            self.assertIn("--require-notarized", calls)
            self.assertNotIn("archive", calls)
            self.assertTrue((root / "dist/fesilent-mouse.app").is_dir())
            self.assertIn("--macos-app", result.stderr)


if __name__ == "__main__":
    unittest.main()
