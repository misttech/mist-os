#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Unit tests for compute_content_hash.py"""

import os
import subprocess
import sys
import tempfile
import typing as T
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import compute_content_hash as cch


def _git_cmd(git_dir: Path, args: T.Sequence[str | Path]) -> str:
    """Run git command in a given directory. Return output as a string."""
    ret = subprocess.run(
        ["git", "-C", str(git_dir)] + [str(a) for a in args],
        check=True,
        text=True,
        capture_output=True,
    )
    return ret.stdout.strip()


def _setup_first_git_dir(git_dir: Path) -> T.Tuple[str, str]:
    git_dir.mkdir(parents=True)
    _git_cmd(git_dir, ["init"])
    # required to avoid errors on CI build bots when running this test.
    _git_cmd(git_dir, ["config", "--local", "user.email", "test@example.com"])
    _git_cmd(git_dir, ["config", "--local", "user.name", "Test User"])

    (git_dir / "foo").write_text("1")
    _git_cmd(git_dir, ["add", "foo"])
    _git_cmd(git_dir, ["commit", "-m", "first commit"])
    first_commit = _git_cmd(git_dir, ["rev-parse", "HEAD"])
    (git_dir / "foo").write_text("2")
    _git_cmd(git_dir, ["commit", "-am", "second commit"])
    second_commit = _git_cmd(git_dir, ["rev-parse", "HEAD"])
    return (first_commit, second_commit)


def _setup_second_git_dir(git_dir: Path, submodule_path: Path) -> str:
    git_dir.mkdir(parents=True)
    _git_cmd(git_dir, ["init"])
    # required to avoid errors on CI build bots when running this test.
    _git_cmd(git_dir, ["config", "--local", "user.email", "test@example.com"])
    _git_cmd(git_dir, ["config", "--local", "user.name", "Test User"])

    # Add a submodule. Using -c protocol.file.allow=always
    # is required to avoid an error message that says:
    # "fatal: transport 'file' not allowed".
    # See https://github.com/flatpak/flatpak-builder/issues/495#issuecomment-1297413908
    _git_cmd(
        git_dir,
        [
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            submodule_path,
            "sub1",
        ],
    )
    _git_cmd(git_dir, ["commit", "-m", "first commit"])
    return _git_cmd(git_dir, ["rev-parse", "HEAD"])


class ComputeContentHashTest(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self._dir = Path(self._td.name)
        self.datadir = self._dir / "data"
        self.datadir.mkdir(parents=True)
        (self.datadir / "README.txt").write_text("Example input file.\n")
        (self.datadir / "subdir").mkdir()
        (self.datadir / "subdir" / "data").write_text("42")
        (self.datadir / "data.link").symlink_to("subdir/data")

        self.prebuiltdir = self._dir / "cipd_prebuilt"
        self.prebuiltdir.mkdir()
        (self.prebuiltdir / ".versions").mkdir()
        (self.prebuiltdir / ".versions" / "bar.cipd_version").write_text(
            "version 1.0"
        )

    def tearDown(self):
        self._td.cleanup()

    def _setup_git_repos(self):
        self.gitdir1 = self._dir / "git_project1"
        self.first_commit, self.second_commit = _setup_first_git_dir(
            self.gitdir1
        )

        self.gitdir2 = self._dir / "git_project2"
        self.first_commit2 = _setup_second_git_dir(self.gitdir2, self.gitdir1)

    def test_file_content_hash(self):
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.datadir / "README.txt")
        self.assertEqual(
            descriptor, "F49d18153ca1983adefbe9c3a2cb91676c73b1024"
        )

    def test_symlink_content_hash(self):
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.datadir / "data.link")
        self.assertEqual(descriptor, "Ssubdir/data")

    def test_directory_content_hash(self):
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.datadir)
        self.assertEqual(
            descriptor,
            """D
 README.txt F49d18153ca1983adefbe9c3a2cb91676c73b1024
 data.link Ssubdir/data
 subdir/data F92cfceb39d57d914ed8b14d0e37643de0797ae56
""",
        )

    def test_cipd_content_hash(self):
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.prebuiltdir)
        self.assertEqual(
            descriptor,
            """D
 .versions/bar.cipd_version F105a6be87e08ee0d67f397a498315de3afaba803
""",
        )
        fstate = cch.FileState(cipd_names=["bar"])
        descriptor = fstate.process_source_path(self.prebuiltdir)
        self.assertEqual(
            descriptor, "F105a6be87e08ee0d67f397a498315de3afaba803"
        )

    def test_git_content_hash_no_submodules(self):
        self._setup_git_repos()

        # on branch on first git repository.
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.gitdir1)
        self.assertEqual(descriptor, "G" + self.second_commit)

        # detached HEAD on first git repository.
        _git_cmd(self.gitdir1, ["checkout", self.first_commit])
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.gitdir1)
        self.assertEqual(descriptor, "G" + self.first_commit)

        # on branch on second git repository + on branch in submodule
        fstate = cch.FileState()
        descriptor = fstate.process_source_path(self.gitdir2)
        self.assertEqual(descriptor, "G" + self.first_commit2)


if __name__ == "__main__":
    unittest.main()
