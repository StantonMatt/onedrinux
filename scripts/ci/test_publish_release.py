"""Exercise tag ownership, clobber behavior and fail-closed publication offline."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

spec = importlib.util.spec_from_file_location("publish_release", Path(__file__).with_name("publish-release.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FakeGitLab:
    base = "https://gitlab.example/api/v4/projects/1"

    def __init__(self):
        self.tag = None
        self.release = None
        self.writes = []

    def request(self, method, path, data=None, missing=False):
        if method == "GET":
            return self.tag if "/repository/tags/" in path else self.release
        self.writes.append((method, path, data))
        if method == "POST" and path == "/releases":
            self.tag = {"commit": {"id": data["ref"]}}
            self.release = {**data, "commit": self.tag["commit"]}
            for i, link in enumerate(self.release["assets"]["links"]):
                link["id"] = i + 1
            return self.release
        if method == "PUT" and "/assets/links/" in path:
            link_id = int(path.rsplit("/", 1)[1])
            for link in self.release["assets"]["links"]:
                if link["id"] == link_id:
                    link.update(data)
        return {}


class PublicationTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        old = Path.cwd()
        root = Path(self.temp.name)
        (root / "scripts").mkdir()
        shutil.copy(Path(__file__).resolve().parents[1] / "version.sh", root / "scripts/version.sh")
        os.chdir(root)
        self.addCleanup(os.chdir, old)
        Path("Cargo.toml").write_text('[workspace.package]\nversion = "0.2.3"\n')
        for args in (("init", "-q"), ("add", "."), ("-c", "user.name=CI test", "-c", "user.email=ci@example.invalid", "commit", "-qm", "Release fixture")):
            subprocess.run(["git", *args], check=True)
        self.sha = module.git("rev-parse", "HEAD")
        self.env = patch.dict(os.environ, CI_COMMIT_SHA=self.sha, CI_JOB_ID="10", release_tag="", RELEASE_SHA="")
        self.env.start()
        os.environ.pop("RELEASE_SHA")
        self.addCleanup(self.env.stop)
        for fedora in (44, 45):
            directory = Path(f"dist/fc{fedora}")
            directory.mkdir(parents=True)
            (directory / "release-commit").write_text(self.sha)
            for name in ("konedrive", "konedrive-kde", "source"):
                (directory / f"{name}-0.2.3.fc{fedora}.rpm").write_bytes(b"rpm fixture")
        self.api = FakeGitLab()

    def test_create_then_retry_reuses_tag_and_replaces_seven_links(self):
        module.publish(self.api)
        self.assertEqual(self.api.tag["commit"]["id"], self.sha)
        links = self.api.release["assets"]["links"]
        self.assertEqual(len(links), 7)
        self.assertEqual(len(Path("dist/SHA256SUMS").read_text().splitlines()), 6)
        self.assertTrue(all("/0.2.3-10/" in link["url"] for link in links))
        os.environ["CI_JOB_ID"] = "11"
        module.publish(self.api)
        self.assertTrue(all("/0.2.3-11/" in link["url"] for link in links))
        creates = [w for w in self.api.writes if w[:2] == ("POST", "/releases")]
        self.assertEqual(len(creates), 1)
        self.assertEqual(creates[0][2]["ref"], self.sha)
        self.assertEqual(len([w for w in self.api.writes if "/assets/links/" in w[1]]), 7)

    def test_tag_on_another_commit_prevents_every_write(self):
        self.api.tag = {"commit": {"id": "0" * 40}}
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_incomplete_artifacts_prevent_every_write(self):
        next(Path("dist").glob("fc*/*.rpm")).unlink()
        with self.assertRaisesRegex(RuntimeError, "six distinct RPMs"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_checkout_mismatch_prevents_every_write(self):
        os.environ["CI_COMMIT_SHA"] = "0" * 40
        with self.assertRaisesRegex(RuntimeError, "checked-out commit"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_republish_requires_existing_remote_tag(self):
        os.environ["release_tag"] = "v0.2.3"
        with self.assertRaisesRegex(RuntimeError, "matching existing release tag"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])
        self.api.tag = {"commit": {"id": self.sha}}
        module.publish(self.api)
        self.assertEqual(self.api.release["tag_name"], "v0.2.3")

    def test_disagreeing_build_commits_prevent_every_write(self):
        Path("dist/fc44/release-commit").write_text("0" * 40)
        with self.assertRaisesRegex(RuntimeError, "same release commit"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_only_404_is_treated_as_missing(self):
        with patch.dict(os.environ, CI_API_V4_URL="https://gitlab.example/api/v4", CI_PROJECT_ID="1", CI_JOB_TOKEN="fake-test-token"):
            api = module.GitLab()
        for code in (403, 500):
            with patch.object(module, "urlopen", side_effect=HTTPError(api.base, code, "failure", {}, None)):
                with self.assertRaisesRegex(RuntimeError, f"HTTP {code}"):
                    api.request("GET", "/releases/v0.2.3", missing=True)
        with patch.object(module, "urlopen", side_effect=HTTPError(api.base, 404, "missing", {}, None)):
            self.assertIsNone(api.request("GET", "/releases/v0.2.3", missing=True))


if __name__ == "__main__":
    unittest.main()
