# Checks against OneDrive

These tools are built and unit-tested without any account. They have **not** been run against
an account for the remote namespace hardening change.

The existing harness is documented in [writes.md §8.1](../../docs/design/writes.md#81-the-guarded-harness).
Its `write_test_drive_ids` allowlist, expected-drive check, account size limits, run-folder-only
wire guard, request and byte caps and owner-only token-file rules remain in force. The allowlist
is empty by default. Never add a real personal drive to run write checks.

For the person doing the later test-account measurement, after explicitly obtaining its token
in an owner-only 0600 file using the documented dev export:

```
cargo run -p konedrive-write-test --bin konedrive-write-test -- \
  --graph-test-drive TEST_DRIVE_ID --graph-token /path/to/test-rw.token \
  --daemon-config /path/to/test-config.toml --only remote
```

This checks folder cTags in GET and delta after nested descendant creation, editing and removal,
412 on DELETE with each stale folder cTag, zero-byte PUT create conflict behavior and If-Match,
and `createdBy.application.id` matching the configured client. It writes only below the run's
folder and puts that folder into the recycle bin at cleanup. A failed stale DELETE check stops
that check immediately. Do not enable real writes based on fake-server results.

The sender requires folder cTags. Missing folder cTags safely leave deletion blocked.

# Read-only metadata sample

`metadata-probe` uses the existing Graph client for GET requests only: expected drive id and
delta metadata. It never opens file content, follows download URLs, writes items, reaches the
session bus or wallet, or reads daemon configuration. Names, item ids, tags, hashes and tokens
are not printed; output is aggregate presence counts. Use a read-only token file (0600, owned
by the invoking user). The explicit expected drive must match before enumeration begins.

```
cargo run -p konedrive-write-test --bin metadata-probe -- \
  --graph-drive EXPECTED_DRIVE_ID --graph-token /path/to/read-only.token --sample 100
```

At most N distinct files and N distinct folders are sampled in delta order, with a hard bound
of 1000 pages. This is a presence probe, not a random estimate or proof of tag semantics. A small
drive or a page bound can yield fewer items; the output says whether enumeration ended.
The token is read only from the explicit file and never printed. Account access is a manual,
separately authorized action; neither command above has been run during implementation.
