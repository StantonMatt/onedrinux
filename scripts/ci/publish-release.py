#!/usr/bin/env python3
"""Publish verified RPMs to GitHub; never move an existing release tag."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from urllib.parse import quote


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


class GitHub:
    def __init__(self):
        self.repo = os.environ["GH_REPO"]

    def request(self, method, path, data=None, missing=False):
        command = ["gh", "api", "--method", method, f"repos/{self.repo}{path}"]
        if data is not None:
            command += ["--input", "-"]
        result = subprocess.run(command, input=json.dumps(data) if data is not None else None,
                                text=True, capture_output=True, check=False)
        if result.returncode:
            if missing and "(HTTP 404)" in result.stderr:
                return None
            raise RuntimeError(f"GitHub {method} {path} failed: {result.stderr.strip()}")
        return json.loads(result.stdout) if result.stdout else None

    def upload(self, tag, files):
        subprocess.run(["gh", "release", "upload", tag, "--repo", self.repo,
                        "--clobber", *map(str, files)], check=True)


def verify_tag(api, tag, sha):
    existing = api.request("GET", f"/git/ref/tags/{quote(tag, safe='')}", missing=True)
    if existing is not None:
        obj = existing["object"]
        while obj["type"] == "tag":
            obj = api.request("GET", f"/git/tags/{obj['sha']}")["object"]
        if obj["type"] != "commit" or obj["sha"] != sha:
            raise RuntimeError(f"{tag} already exists on {obj['sha']}, not {sha}")
    return existing


def publish(api):
    version = subprocess.check_output(["scripts/version.sh", "release"], text=True).strip()
    sha = os.environ["RELEASE_SHA"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) or git("rev-parse", "HEAD") != sha:
        raise RuntimeError("Release version or checked-out commit does not match the pipeline")
    tag = "v" + version
    existing_tag = verify_tag(api, tag, sha)
    requested_tag = os.environ.get("RELEASE_TAG", "")
    if requested_tag and (requested_tag != tag or existing_tag is None):
        raise RuntimeError("Republishing requires the matching existing release tag")
    commits = list(Path("dist").glob("*/release-commit"))
    if len(commits) != 2 or any(p.read_text().strip() != sha for p in commits):
        raise RuntimeError("Both Fedora builds must contain the same release commit")
    rpms = sorted(Path("dist").glob("*/*.rpm"))
    if len(rpms) != 6 or len({p.name for p in rpms}) != 6:
        raise RuntimeError("Expected six distinct RPMs from the two Fedora builds")
    release = api.request("GET", f"/releases/tags/{tag}", missing=True)
    sums = Path("dist/SHA256SUMS")
    sums.write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in rpms))
    if existing_tag is None:
        api.request("POST", "/git/refs", {"ref": "refs/tags/" + tag, "sha": sha})
    # Recheck ownership before any release or asset write.
    if not verify_tag(api, tag, sha):
        raise RuntimeError("GitHub did not create the tag")
    if release is None:
        notes = Path(f"docs/release-notes/{version}.md")
        if notes.exists():
            description = notes.read_text()
        else:
            previous = subprocess.check_output(["scripts/version.sh", "previous", version], text=True).strip()
            commits = git("log", "--format=- %s (%h)", f"{previous}..{sha}" if previous else sha)
            description = f"Changes since {previous or 'the start'}:\n\n{commits}\n"
        release = api.request("POST", "/releases", {
            "tag_name": tag, "name": "KOneDrive " + version,
            "body": description, "draft": True,
        })
    api.upload(tag, [*rpms, sums])
    if release["draft"]:
        api.request("PATCH", f"/releases/{release['id']}", {"draft": False})
    print(f"Published {tag} on {sha}, with seven assets")


if __name__ == "__main__":
    publish(GitHub())
