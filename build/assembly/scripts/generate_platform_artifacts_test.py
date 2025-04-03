# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import json
import os
import sys
import tempfile
import unittest

import generate_platform_artifacts


class VersionTest(unittest.TestCase):
    def test_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            aib_list_path = os.path.join(tmpdir, "aib_list")
            with open(aib_list_path, "w") as file:
                file.write("[]")

            outdir_path = os.path.join(tmpdir, "outdir")
            os.makedirs(outdir_path)

            depfile_path = os.path.join(tmpdir, "depfile")

            sys.argv = [
                "",
                "--aib-list",
                aib_list_path,
                "--outdir",
                outdir_path,
                "--depfile",
                depfile_path,
                "--version",
                "fake_version_123",
            ]
            generate_platform_artifacts.main()

            expected = {"release_version": "fake_version_123"}

            result_path = os.path.join(
                tmpdir, "outdir", "platform_artifacts.json"
            )
            with open(result_path, "r") as file:
                actual = json.load(file)

            self.assertEqual(expected, actual)

    def test_unversioned(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            aib_list_path = os.path.join(tmpdir, "aib_list")
            with open(aib_list_path, "w") as file:
                file.write("[]")

            outdir_path = os.path.join(tmpdir, "outdir")
            os.makedirs(outdir_path)

            depfile_path = os.path.join(tmpdir, "depfile")

            sys.argv = [
                "",
                "--aib-list",
                aib_list_path,
                "--outdir",
                outdir_path,
                "--depfile",
                depfile_path,
            ]
            generate_platform_artifacts.main()

            expected = {"release_version": "unversioned"}

            result_path = os.path.join(
                tmpdir, "outdir", "platform_artifacts.json"
            )
            with open(result_path, "r") as file:
                actual = json.load(file)

            self.assertEqual(expected, actual)
