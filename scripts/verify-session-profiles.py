#!/usr/bin/env python3
"""Verify profile sharing through two real binaries using synthetic local data.

Requires Python 3.11 or later and only the standard library. The fixture stays
on disk with commands, reports, binary hashes, and the generated LaunchAgent.
Neither binary receives real account data or the user's home directory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
from typing import BinaryIO


def _uuid(number: int) -> str:
    return f"{number:08x}-0000-4000-8000-{number:012x}"


def _digest(path: Path) -> str:
    source: BinaryIO
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def _snapshot(root: Path) -> dict[str, bytes]:
    return {str(path): path.read_bytes() for path in root.rglob("*") if path.is_file()}


def _object(text: str) -> dict[str, object]:
    value: object = json.loads(text)
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


class _Fixture:
    """Own the synthetic filesystem and invoke binaries without live user data."""

    def __init__(self) -> None:
        self.base: Path = Path(tempfile.mkdtemp(prefix="claude-session-profiles-")).resolve()
        self.primary: Path = self.base / "primary"
        self.extra: Path = self.base / "extra"
        self.future: Path = self.base / "future"
        self.projects: Path = self.base / "projects"
        self.state: Path = self.base / "state"
        self.home: Path = self.base / "home"
        self.cwd: Path = self.base / "cwd"
        self.reports: list[dict[str, object]] = []
        for directory in [self.primary, self.extra, self.projects, self.state, self.home, self.cwd]:
            directory.mkdir()
        self.env: dict[str, str] = {
            **os.environ,
            "HOME": str(self.home),
            "CLAUDE_ACCOUNT_KIT_HOME": str(self.state),
            "CLAUDE_HOME": str(self.base / "config"),
            "CLAUDE_CONFIG_DIR": str(self.base / "config"),
        }

    def partition(self, root: Path, account: int, org: int) -> Path:
        """Create a synthetic account partition as Desktop would after login."""
        directory: Path = root / "claude-code-sessions" / _uuid(account) / _uuid(org)
        directory.mkdir(parents=True)
        return directory

    @staticmethod
    def wrapper(partition: Path, number: int) -> Path:
        """Return a wrapper path without creating it."""
        return partition / f"local_{_uuid(number)}.json"

    def write_wrapper(
        self, partition: Path, number: int, cli: int, *, prior: tuple[int, ...] = (),
        timestamp: int = 1790000000000,
    ) -> bytes:
        """Write a synthetic Desktop original and return its preservation bytes."""
        value: dict[str, object] = {
            "sessionId": f"local_{_uuid(number)}", "cliSessionId": _uuid(cli),
            "cwd": str(self.cwd), "originCwd": str(self.cwd),
            "createdAt": 1790000000000, "lastActivityAt": timestamp,
            "title": "Fixed fixture", "isArchived": False,
        }
        if prior:
            value["priorCliSessionIds"] = [_uuid(number) for number in prior]
        path: Path = self.wrapper(partition, number)
        path.write_text(json.dumps(value) + "\n", encoding="utf-8")
        return path.read_bytes()

    def transcript(self, number: int) -> Path:
        """Write a synthetic transcript with an existing local working directory."""
        project: Path = self.projects / "fixture"
        project.mkdir(exist_ok=True)
        path: Path = project / f"{_uuid(number)}.jsonl"
        value: dict[str, object] = {
            "type": "user", "sessionId": _uuid(number), "cwd": str(self.cwd),
            "timestamp": "2026-10-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Synthetic fixture"},
        }
        path.write_text(json.dumps(value) + "\n", encoding="utf-8")
        return path

    def run(self, binary: Path, roots: tuple[Path, ...] = (), *, dry_run: bool = False) -> dict[str, object]:
        """Run sync, preserving every transcript and all state during dry run."""
        args: list[str] = [
            str(binary), "sessions", "sync", "--desktop-root", str(self.primary),
            "--projects-root", str(self.projects), "--json",
        ]
        for root in roots:
            args += ["--additional-desktop-root", str(root)]
        if dry_run:
            args.append("--dry-run")
        before_transcripts: dict[str, bytes] = _snapshot(self.projects)
        before_state: dict[str, bytes] = _snapshot(self.state)
        result: subprocess.CompletedProcess[str] = subprocess.run(
            args, env=self.env, text=True, capture_output=True, check=True, timeout=30,
        )
        assert before_transcripts == _snapshot(self.projects), "sync modified a transcript"
        if dry_run:
            assert before_state == _snapshot(self.state), "dry run modified state"
        report: dict[str, object] = _object(result.stdout)
        self.reports.append({"command": args, "report": report})
        (self.base / "reports.json").write_text(json.dumps(self.reports, indent=2) + "\n", encoding="utf-8")
        return report


def _verify_sharing(fixture: _Fixture, old: Path, new: Path) -> None:
    a: Path = fixture.partition(fixture.primary, 1, 11)
    b: Path = fixture.partition(fixture.primary, 2, 22)
    x1: Path = fixture.transcript(201)
    fixture.write_wrapper(a, 101, 201)
    fixture.run(old)
    legacy_b: bytes = fixture.wrapper(b, 101).read_bytes()

    # Exact bytes and the same account/org/path must not transfer ownership
    # from the primary ledger to a Desktop original in another profile.
    b_extra: Path = fixture.partition(fixture.extra, 2, 22)
    c_extra: Path = fixture.partition(fixture.extra, 3, 33)
    fixture.wrapper(b_extra, 101).write_bytes(legacy_b)
    fixture.transcript(202)
    original_c: bytes = fixture.write_wrapper(c_extra, 102, 202)
    marker: Path = b_extra / f"deleted_local_{_uuid(102)}"
    marker.write_text("", encoding="utf-8")
    x2: Path = fixture.transcript(203)
    original_a: bytes = fixture.write_wrapper(a, 101, 203, prior=(201,), timestamp=1790003600000)
    for root in [fixture.primary, fixture.extra]:
        (root / "auth-sentinel.bin").write_bytes(b"untouched-auth-fixture")
    roots: tuple[Path, ...] = (
        fixture.extra, fixture.extra / ".." / "extra", fixture.primary, fixture.future,
    )
    report: dict[str, object] = fixture.run(new, roots, dry_run=True)
    assert report["partitions"] == 4, report
    assert fixture.wrapper(b, 101).read_bytes() == legacy_b
    assert not fixture.wrapper(c_extra, 101).exists()
    report = fixture.run(new, roots)
    assert report["partitions"] == 4, report
    assert _object(fixture.wrapper(b, 101).read_text())["cliSessionId"] == _uuid(203)
    assert fixture.wrapper(b_extra, 101).read_bytes() == legacy_b
    assert _object(fixture.wrapper(c_extra, 101).read_text())["cliSessionId"] == _uuid(203)
    assert _object(fixture.wrapper(a, 102).read_text())["cliSessionId"] == _uuid(202)
    assert not fixture.wrapper(b, 102).exists() and not fixture.wrapper(b_extra, 102).exists()
    assert fixture.wrapper(a, 101).read_bytes() == original_a
    assert fixture.wrapper(c_extra, 102).read_bytes() == original_c
    assert not fixture.future.exists()
    report = fixture.run(new, roots)
    assert report["applied"] == {"add": 0, "refresh": 0, "remove": 0}, report

    fixture.extra.rename(fixture.base / "temporarily-absent")
    report = fixture.run(new, (fixture.extra, fixture.future))
    assert report["applied"] == {"add": 0, "refresh": 0, "remove": 0}, report
    (fixture.base / "temporarily-absent").rename(fixture.extra)

    # Remove only owned orphan copies; both Desktop originals must survive.
    x1.unlink()
    x2.unlink()
    fixture.run(new, roots)
    assert not fixture.wrapper(b, 101).exists() and not fixture.wrapper(c_extra, 101).exists()
    assert fixture.wrapper(b_extra, 101).read_bytes() == legacy_b
    assert fixture.wrapper(a, 101).read_bytes() == original_a
    assert fixture.wrapper(c_extra, 102).read_bytes() == original_c and marker.exists()
    for root in [fixture.primary, fixture.extra]:
        assert (root / "auth-sentinel.bin").read_bytes() == b"untouched-auth-fixture"
    future_partition: Path = fixture.partition(fixture.future, 4, 44)
    fixture.run(new, roots)
    assert fixture.wrapper(future_partition, 102).exists()
    assert not fixture.wrapper(future_partition, 101).exists()


def _verify_plist(fixture: _Fixture, new: Path) -> None:
    args: list[str] = [
        str(new), "sessions", "background", "plist",
        "--additional-desktop-root", str(fixture.extra),
        "--additional-desktop-root", str(fixture.future),
    ]
    result: subprocess.CompletedProcess[bytes] = subprocess.run(
        args, env=fixture.env, capture_output=True, check=True, timeout=30,
    )
    plist: dict[str, object] = plistlib.loads(result.stdout)
    program_args: object = plist["ProgramArguments"]
    assert isinstance(program_args, list) and program_args[-4:] == args[-4:]
    assert plist["WatchPaths"] == [
        str(fixture.home / "Library/Application Support/Claude/claude-code-sessions"),
        str(fixture.extra / "claude-code-sessions"), str(fixture.future / "claude-code-sessions"),
    ]
    (fixture.base / "launch-agent.plist").write_bytes(result.stdout)


def main() -> None:
    """Verify explicit old/new binaries and print the retained artifact location."""
    if not __debug__:
        raise RuntimeError("run without -O so fixture checks remain enabled")
    parser: argparse.ArgumentParser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-binary", required=True, type=Path, help="Backed-up pre-change binary")
    parser.add_argument("--new-binary", required=True, type=Path, help="Built multi-profile binary")
    args: argparse.Namespace = parser.parse_args()
    old: Path = args.old_binary.expanduser().resolve(strict=True)
    new: Path = args.new_binary.expanduser().resolve(strict=True)
    old_sha256: str = _digest(old)
    new_sha256: str = _digest(new)
    if old_sha256 == new_sha256:
        parser.error("migration requires different old and new binaries")
    fixture: _Fixture = _Fixture()
    print(f"Fixture artifacts: {fixture.base}", flush=True)
    _verify_sharing(fixture, old, new)
    _verify_plist(fixture, new)
    result: dict[str, object] = {
        "status": "passed", "base": str(fixture.base),
        "old_binary": str(old), "old_binary_sha256": old_sha256,
        "new_binary": str(new), "new_binary_sha256": new_sha256,
        "checks": [
            "old-binary ledger migration", "cross-profile union",
            "same account/org original ownership isolation", "same-account cross-profile tombstone",
            "duplicate-root deduplication", "dry-run preservation", "second-run idempotence",
            "missing-profile preservation", "tool-owned orphan removal only",
            "deferred future-profile discovery", "auth and transcript preservation",
            "LaunchAgent root persistence and watches",
        ],
    }
    (fixture.base / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
