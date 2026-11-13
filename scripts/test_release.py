import json
from pathlib import Path
import plistlib
import tempfile
import unittest

import release


class VersionTests(unittest.TestCase):
    def test_accepts_stable_versions(self):
        self.assertEqual(release.version_parts("1.20.3"), (1, 20, 3))

    def test_rejects_prereleases_and_leading_zeros(self):
        for version in ("1.0.0-rc.1", "01.0.0", "1.0"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.version_parts(version)

    def test_tag_must_match_the_workspace_version(self):
        release.validate_tag("v1.2.3", "1.2.3")
        with self.assertRaises(ValueError):
            release.validate_tag("1.2.3", "1.2.3")


class PlistTests(unittest.TestCase):
    def test_writes_version_and_build_number(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "Info.plist"
            release.write_plist(output, "1.2.3", "42")
            metadata = plistlib.loads(output.read_bytes())
        self.assertEqual(metadata["CFBundleShortVersionString"], "1.2.3")
        self.assertEqual(metadata["CFBundleVersion"], "42")
        self.assertEqual(metadata["CFBundleExecutable"], "simplymail")

    def test_rejects_invalid_build_numbers(self):
        with self.assertRaises(ValueError):
            release.write_plist(Path("unused"), "1.2.3", "0")


class ConfigTests(unittest.TestCase):
    def test_reads_the_crate_names_bumped_in_the_lockfile(self):
        config = json.loads((release.ROOT / "release-please-config.json").read_text())
        names = release.release_please_lock_names(config)
        self.assertIn("simplymail", names)
        self.assertIn("simplymail-app", names)


class FormulaTests(unittest.TestCase):
    VERSION = "1.2.3"

    def sums(self, version=VERSION):
        names = list(release.formula_assets(version).values()) + [f"simplymail_{version}_amd64.deb"]
        return "".join(f"{str(i) * 64}  {name}\n" for i, name in enumerate(names))

    def test_points_each_platform_at_its_asset(self):
        formula = release.render_formula(self.VERSION, release.parse_sums(self.sums()))
        base = "https://github.com/mattsverse/simplymail/releases/download/v1.2.3/"
        self.assertIn(f'url "{base}SimplyMail-1.2.3-universal.zip"\n    sha256 "{"0" * 64}"', formula)
        self.assertIn(f'url "{base}simplymail-1.2.3-aarch64-unknown-linux-gnu.tar.gz"\n    sha256 "{"1" * 64}"', formula)
        self.assertIn(f'url "{base}simplymail-1.2.3-x86_64-unknown-linux-gnu.tar.gz"\n    sha256 "{"2" * 64}"', formula)
        self.assertIn('  version "1.2.3"\n', formula)

    def test_requires_every_asset(self):
        with self.assertRaises(ValueError):
            release.render_formula(self.VERSION, {})

    def test_rejects_malformed_sums(self):
        with self.assertRaises(ValueError):
            release.parse_sums("not a checksum  file.zip\n")

    def test_refuses_to_downgrade_the_tap(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "Formula/simplymail.rb"
            release.write_formula(output, "1.10.0", self.sums("1.10.0"))
            with self.assertRaises(ValueError):
                release.write_formula(output, "1.9.0", self.sums("1.9.0"))
            release.write_formula(output, "1.10.0", self.sums("1.10.0"))
            self.assertIn('version "1.10.0"', output.read_text())


if __name__ == "__main__":
    unittest.main()
