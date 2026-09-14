"""Exercise release packaging and installation without compiling or networking."""

import hashlib
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-gnu"
TAG = "v1.2.3.123"
BINARIES = {"chaos", "alcatraz", "chaos_journald", "chaos-forkve-wrapper"}
BASH = shutil.which("bash") or "bash"


class ReleaseTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="chaos release ")
        self.addCleanup(temp.cleanup)
        self.work = Path(temp.name)
        (self.work / "scripts").mkdir()
        for name in ("build-release.sh", "dist-install.sh"):
            shutil.copyfile(ROOT / "scripts" / name, self.work / "scripts" / name)
        self.tools = self.work / "tools"
        self.tools.mkdir()
        self.env = {
            "PATH": f"{self.tools}:{os.defpath}",
            "HOME": str(self.work),
            "TMPDIR": str(self.work),
            "CARGO_TARGET_DIR": str(self.work / "build output"),
            "CHAOS_BUILD_TS": "123",
            "CHAOS_VERSION": TAG,
            "CHAOS_INSTALL_DIR": str(self.work / "installed"),
            "TEST_TARGET": TARGET,
            "TEST_CARGO_LOG": str(self.work / "cargo.log"),
            "TEST_CURL_LOG": str(self.work / "curl.log"),
            "TEST_BUNDLE_DIR": str(self.work),
        }
        self.tool("cargo", r'''
printf '%s\n' "$*" >> "$TEST_CARGO_LOG"
[ "${TEST_CARGO_FAIL:-0}" = 0 ] || exit 1
dir="$CARGO_TARGET_DIR/$TEST_TARGET/release"
mkdir -p "$dir"
case " $* " in
    *" --bin chaos "*)
        mode=headless
        case " $* " in *" --features tui "*) mode="tui --no-alt-screen" ;; esac
        case "${TEST_WRONG_FLAVOR:-}" in
            1) mode=wrong ;;
            headless) mode="tui --no-alt-screen" ;;
        esac
        printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$mode" > "$dir/chaos"
        ;;
    *)
        for bin in alcatraz chaos_journald chaos-forkve-wrapper; do
            [ "$bin" != "${TEST_MISSING_HELPER:-}" ] || continue
            printf '#!/bin/sh\necho %s\n' "$bin" > "$dir/$bin"
        done
        ;;
esac
chmod +x "$dir"/*
''')
        self.tool("strip", "exit 0\n")
        self.tool("uname", 'case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n')
        self.tool("curl", r'''
dest=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) dest=$2; shift 2 ;;
        *) url=$1; shift ;;
    esac
done
printf '%s\n' "$url" >> "$TEST_CURL_LOG"
cp "$TEST_BUNDLE_DIR/${url##*/}" "$dest"
''')

    def tool(self, name, body):
        path = self.tools / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)

    def run_cmd(self, *args, overrides=None, cwd=None, success=True):
        result = subprocess.run(
            args, cwd=cwd or self.work, env={**self.env, **(overrides or {})},
            capture_output=True, text=True, timeout=20,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def build(self, **kwargs):
        return self.run_cmd(BASH, "scripts/build-release.sh", TARGET, TAG, **kwargs)

    def archive(self, flavor):
        prefix = "chaos-headless" if flavor == "headless" else "chaos"
        return self.work / f"{prefix}-{TAG}-{TARGET}.tar.gz"

    def test_both_archives_and_installers(self):
        self.build()
        calls = [shlex.split(line) for line in (self.work / "cargo.log").read_text().splitlines()]
        self.assertEqual(len(calls), 3)  # Helpers once, then each CLI separately.
        def packages(args):
            return [args[i + 1] for i, arg in enumerate(args) if arg == "-p"]

        self.assertEqual(packages(calls[0]), ["alcatraz", "chaos_journald", "chaos-doas"])
        for args in calls:
            self.assertIn("--locked", args)
            self.assertIn("--release", args)
            self.assertNotIn("--workspace", args)
            self.assertNotIn("--all-features", args)
            self.assertEqual(args[args.index("--target") + 1], TARGET)
        for flavor, args in zip(("tui", "headless"), calls[1:]):
            with self.subTest(flavor=flavor):
                self.assertEqual(packages(args), ["chaos-cli"])
                self.assertIn("--no-default-features", args)
                self.assertEqual("--features" in args, flavor == "tui")
                if flavor == "tui":
                    self.assertEqual(args[args.index("--features") + 1], "tui")
                archive = self.archive(flavor)
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                self.assertEqual(
                    Path(f"{archive}.sha256").read_text(), f"{digest}  {archive.name}\n",
                )
                with tarfile.open(archive) as bundle:
                    files = {m.name.removeprefix("./"): m for m in bundle if m.isfile()}
                    self.assertEqual(set(files), BINARIES | {"install.sh"})
                    for name in BINARIES:
                        self.assertTrue(files[name].mode & 0o111, name)
                unpacked = self.work / flavor
                unpacked.mkdir()
                self.run_cmd("tar", "xzf", str(archive), "-C", str(unpacked))
                dest = self.work / f"bundled-{flavor}"
                self.run_cmd("sh", "install.sh", str(dest), cwd=unpacked)
                self.assertEqual({p.name for p in dest.iterdir()}, BINARIES)
                help_text = self.run_cmd(str(dest / "chaos"), "--help").stdout
                self.assertEqual("--no-alt-screen" in help_text, flavor == "tui")

        for flavor in (None, "tui", "headless"):
            with self.subTest(installer_flavor=flavor):
                overrides = {} if flavor is None else {"CHAOS_FLAVOR": flavor}
                self.run_cmd("sh", str(ROOT / "install.sh"), overrides=overrides)
                dest = Path(self.env["CHAOS_INSTALL_DIR"])
                help_text = self.run_cmd(str(dest / "chaos"), "--help").stdout
                self.assertEqual("--no-alt-screen" in help_text, flavor != "headless")
                downloads = (self.work / "curl.log").read_text().splitlines()
                self.assertTrue(downloads[-1].endswith(self.archive(flavor).name + ".sha256"))

    def test_invalid_flavor_fails_before_downloading(self):
        result = self.run_cmd(
            "sh", str(ROOT / "install.sh"), overrides={"CHAOS_FLAVOR": "typo"}, success=False,
        )
        self.assertIn("CHAOS_FLAVOR must be", result.stderr)
        self.assertFalse((self.work / "curl.log").exists())

    def test_headless_checksum_mismatch_installs_nothing(self):
        self.build()
        archive = self.archive("headless")
        archive.write_bytes(archive.read_bytes() + b"tampered")
        result = self.run_cmd(
            "sh", str(ROOT / "install.sh"), overrides={"CHAOS_FLAVOR": "headless"}, success=False,
        )
        self.assertIn("SHA-256 mismatch", result.stderr)
        self.assertFalse(Path(self.env["CHAOS_INSTALL_DIR"]).exists())

    def test_failed_build_publishes_nothing(self):
        self.build(overrides={"TEST_CARGO_FAIL": "1"}, success=False)
        self.assertEqual(list(self.work.glob("*.tar.gz*")), [])

    def test_missing_helper_publishes_nothing(self):
        self.build(overrides={"TEST_MISSING_HELPER": "alcatraz"}, success=False)
        self.assertEqual(list(self.work.glob("*.tar.gz*")), [])

    def test_wrong_feature_surface_publishes_nothing(self):
        result = self.build(overrides={"TEST_WRONG_FLAVOR": "1"}, success=False)
        self.assertIn("wrong CLI feature surface", result.stderr)
        self.assertEqual(list(self.work.glob("*.tar.gz*")), [])

    def test_headless_rejects_tui_leak(self):
        result = self.build(overrides={"TEST_WRONG_FLAVOR": "headless"}, success=False)
        self.assertIn("headless build has the wrong CLI feature surface", result.stderr)
        self.assertFalse(self.archive("headless").exists())

    def test_invalid_build_arguments(self):
        for args in ((), (TARGET,), (TARGET, TAG, "extra"), ("../escape", TAG)):
            with self.subTest(args=args):
                self.run_cmd(BASH, "scripts/build-release.sh", *args, success=False)
                self.assertFalse((self.work / "cargo.log").exists())


if __name__ == "__main__":
    unittest.main()
