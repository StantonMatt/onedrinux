#!/usr/bin/env python3
"""Publish verified RPMs with CI_JOB_TOKEN; never move an existing release tag."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


class GitLab:
    def __init__(self):
        self.base = f'{os.environ["CI_API_V4_URL"]}/projects/{os.environ["CI_PROJECT_ID"]}'
        self.token = os.environ["CI_JOB_TOKEN"]

    def request(self, method, path, data=None, missing=False):
        headers = {"JOB-TOKEN": self.token, "Content-Type": "application/octet-stream"}
        if isinstance(data, dict):
            data = json.dumps(data).encode()
            headers["Content-Type"] = "application/json"
        request = Request(self.base + path, data=data, headers=headers, method=method)
        try:
            with urlopen(request, timeout=120) as response:
                body = response.read()
                return json.loads(body) if body else None
        except HTTPError as error:
            error.close()
            if missing and error.code == 404:
                return None
            # Keep credentials and server bodies out of logs.
            raise RuntimeError(f"GitLab {method} {path}: HTTP {error.code}") from None


def verify_tag(api, tag, sha):
    existing = api.request("GET", f"/repository/tags/{quote(tag, safe='')}", missing=True)
    if existing is not None and existing["commit"]["id"] != sha:
        raise RuntimeError(f"{tag} already exists on {existing['commit']['id']}, not {sha}")
    return existing


def publish(api):
    version = subprocess.check_output(["scripts/version.sh", "release"], text=True).strip()
    sha = os.environ.get("RELEASE_SHA", os.environ["CI_COMMIT_SHA"])
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) or git("rev-parse", "HEAD") != sha:
        raise RuntimeError("Release version or checked-out commit does not match the pipeline")
    tag = "v" + version
    existing_tag = verify_tag(api, tag, sha)
    requested_tag = os.environ.get("release_tag", "")
    if requested_tag and (requested_tag != tag or existing_tag is None):
        raise RuntimeError("Republishing requires the matching existing release tag")
    commits = list(Path("dist").glob("fc*/release-commit"))
    if len(commits) != 2 or any(p.read_text().strip() != sha for p in commits):
        raise RuntimeError("Both Fedora builds must contain the same release commit")
    path = f"/releases/{quote(tag, safe='')}"
    release = api.request("GET", path, missing=True)
    if release and release["commit"]["id"] != sha:
        raise RuntimeError("Release commit does not match the pipeline")

    rpms = sorted(Path("dist").glob("fc*/*.rpm"))
    if len(rpms) != 6 or len({p.name for p in rpms}) != 6:
        raise RuntimeError("Expected six distinct RPMs from the two Fedora builds")
    sums = Path("dist/SHA256SUMS")
    sums.write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in rpms))
    # Each retry has a new job ID: immutable uploads work even if the registry
    # disallows duplicates. Replacing links gives release-upload --clobber semantics.
    package = f'/packages/generic/konedrive/{version}-{os.environ["CI_JOB_ID"]}'
    links = []
    for file in [*rpms, sums]:
        upload = f"{package}/{quote(file.name, safe='')}"
        api.request("PUT", upload, file.read_bytes())
        links.append({"name": file.name, "url": api.base + upload,
                      "direct_asset_path": "/" + file.name, "link_type": "package"})

    if release is None:
        notes = Path(f"docs/release-notes/{version}.md")
        if notes.exists():
            description = notes.read_text()
        else:
            previous = subprocess.check_output(["scripts/version.sh", "previous", version], text=True).strip()
            commits = git("log", "--format=- %s (%h)", f"{previous}..{sha}" if previous else sha)
            description = f"Changes since {previous or 'the start'}:\n\n{commits}\n"
        # The Releases API creates a missing tag at ref using CI_JOB_TOKEN.
        # POST /repository/tags cannot be used with that token.
        release = api.request("POST", "/releases", {
            "tag_name": tag, "ref": sha, "name": "KOneDrive " + version,
            "description": description, "assets": {"links": links},
        })
    else:
        existing = {link["name"]: link for link in release["assets"]["links"]}
        for link in links:
            if link["name"] in existing:
                api.request("PUT", f'{path}/assets/links/{existing[link["name"]]["id"]}', link)
            else:
                api.request("POST", f"{path}/assets/links", link)
    if not verify_tag(api, tag, sha):
        raise RuntimeError("Release API did not create the tag")
    print(f"Published {tag} on {sha}, with {len(links)} download links")


if __name__ == "__main__":
    publish(GitLab())
