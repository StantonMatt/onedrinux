"""Exercise tag ownership, clobber behavior and fail-closed publication offline."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("publish_release", Path(__file__).with_name("publish-release.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FakeGitHub:
    def __init__(self):
        self.tag = None
        self.release = None
        self.writes = []

    def request(self, method, path, data=None, missing=False):
        if method == "GET":
            return self.tag if "/git/ref/" in path else self.release
        self.writes.append((method, path, data))
        if method == "POST" and path == "/git/refs":
            self.tag = {"object": {"type": "commit", "sha": data["sha"]}}
        if method == "POST" and path == "/releases":
            self.release = {**data, "id": 1}
            return self.release
        if method == "PATCH":
            self.release.update(data)
        return {}

    def upload(self, tag, files):
        self.writes.append(("upload", tag, [p.name for p in files]))


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
        self.env = patch.dict(os.environ, RELEASE_SHA=self.sha, RELEASE_TAG="")
        self.env.start()
        self.addCleanup(self.env.stop)
        for fedora in (44, 45):
            directory = Path(f"dist/fc{fedora}")
            directory.mkdir(parents=True)
            (directory / "release-commit").write_text(self.sha)
            for name in ("konedrive", "konedrive-kde", "source"):
                (directory / f"{name}-0.2.3.fc{fedora}.rpm").write_bytes(b"rpm fixture")
        self.api = FakeGitHub()

    def test_create_then_retry_reuses_tag_and_replaces_seven_assets(self):
        module.publish(self.api)
        self.assertEqual(self.api.tag["object"]["sha"], self.sha)
        self.assertFalse(self.api.release["draft"])
        self.assertEqual(len(Path("dist/SHA256SUMS").read_text().splitlines()), 6)
        module.publish(self.api)
        creates = [w for w in self.api.writes if w[:2] == ("POST", "/releases")]
        self.assertEqual(len(creates), 1)
        uploads = [w for w in self.api.writes if w[0] == "upload"]
        self.assertEqual(len(uploads), 2)
        self.assertTrue(all(len(w[2]) == 7 for w in uploads))

    def test_tag_on_another_commit_prevents_every_write(self):
        self.api.tag = {"object": {"type": "commit", "sha": "0" * 40}}
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_incomplete_artifacts_prevent_every_write(self):
        next(Path("dist").glob("fc*/*.rpm")).unlink()
        with self.assertRaisesRegex(RuntimeError, "six distinct RPMs"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_checkout_mismatch_prevents_every_write(self):
        os.environ["RELEASE_SHA"] = "0" * 40
        with self.assertRaisesRegex(RuntimeError, "checked-out commit"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_republish_requires_existing_remote_tag(self):
        os.environ["RELEASE_TAG"] = "v0.2.3"
        with self.assertRaisesRegex(RuntimeError, "matching existing release tag"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])
        self.api.tag = {"object": {"type": "commit", "sha": self.sha}}
        module.publish(self.api)
        self.assertEqual(self.api.release["tag_name"], "v0.2.3")

    def test_disagreeing_build_commits_prevent_every_write(self):
        Path("dist/fc44/release-commit").write_text("0" * 40)
        with self.assertRaisesRegex(RuntimeError, "same release commit"):
            module.publish(self.api)
        self.assertEqual(self.api.writes, [])

    def test_only_404_is_treated_as_missing(self):
        with patch.dict(os.environ, GH_REPO="owner/repo"):
            api = module.GitHub()
        for code in (403, 500):
            result = subprocess.CompletedProcess([], 1, "", f"gh: failure (HTTP {code})")
            with patch.object(module.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(RuntimeError, f"HTTP {code}"):
                    api.request("GET", "/releases/tags/v0.2.3", missing=True)
        result = subprocess.CompletedProcess([], 1, "", "gh: missing (HTTP 404)")
        with patch.object(module.subprocess, "run", return_value=result):
            self.assertIsNone(api.request("GET", "/releases/tags/v0.2.3", missing=True))

    def test_annotated_tag_is_peeled_before_comparing_commits(self):
        class AnnotatedAPI:
            def request(self, method, path, missing=False):
                if "/git/ref/" in path:
                    return {"object": {"type": "tag", "sha": "tag-object"}}
                return {"object": {"type": "commit", "sha": "commit-object"}}
        self.assertIsNotNone(module.verify_tag(AnnotatedAPI(), "v0.2.3", "commit-object"))
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            module.verify_tag(AnnotatedAPI(), "v0.2.3", "wrong-commit")

    def test_failed_upload_leaves_new_release_draft_and_retry_finishes(self):
        with patch.object(self.api, "upload", side_effect=RuntimeError("upload failed")):
            with self.assertRaisesRegex(RuntimeError, "upload failed"):
                module.publish(self.api)
        self.assertTrue(self.api.release["draft"])
        module.publish(self.api)
        self.assertFalse(self.api.release["draft"])


if __name__ == "__main__":
    unittest.main()
