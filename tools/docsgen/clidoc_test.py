#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import sys
import os
import json
import tarfile

clidoc_tarfile = sys.argv.pop()

want_names = [
    "clidoc",
    "clidoc/blobfs-compression.md",
    "clidoc/bootserver.md",
    "clidoc/cmc.md",
    "clidoc/configc.md",
    "clidoc/ffx.md",
    "clidoc/fidl-format.md",
    "clidoc/fidlc.md",
    "clidoc/fidlcat.md",
    "clidoc/fidlgen_cpp.md",
    "clidoc/fidlgen_hlcpp.md",
    "clidoc/fidlgen_rust.md",
    "clidoc/fpublish.md",
    "clidoc/fremote.md",
    "clidoc/fserve.md",
    "clidoc/fssh.md",
    "clidoc/funnel.md",
    "clidoc/minfs.md",
    "clidoc/pm.md",
    "clidoc/symbolizer.md",
    "clidoc/triage.md",
    "clidoc/zbi.md",
    "clidoc/zxdb.md",
]


class CliDocTest(unittest.TestCase):
    """
    Validate the clidoc contents
    """

    def test_tarball_exists(self):
        tarball = clidoc_tarfile
        parents = os.listdir(os.path.dirname(clidoc_tarfile))
        parents += os.listdir(os.path.dirname(os.path.dirname(clidoc_tarfile)))
        self.assertTrue(
            os.path.exists(tarball), f"{tarball} does not exist in {parents}"
        )

    def test_tarball_contents(self):
        tarball = clidoc_tarfile
        got_names = []
        with tarfile.open(tarball, mode="r:gz") as tf:
            got_names = [m.name for m in tf.getmembers()]
        # sort to make it stable
        got_names.sort()
        self.assertEqual(got_names, want_names)

    def test_ffx_contents(self):
        want_subcommands = [
            "component",
            "config",
            "daemon",
            "debug",
            "doctor",
            "emu",
            "net",
            "package",
            "platform",
            "product",
            "repository",
            "sdk",
            "session",
            "target",
            "version",
        ]
        tarball = clidoc_tarfile
        with tarfile.open(tarball, mode="r:gz") as tf:
            ffx_member = tf.getmember("clidoc/ffx.md")
            self.assertIsNotNone(ffx_member)
            ffx_file = tf.extractfile(ffx_member)
            contents = ffx_file.readlines()
        # filter the H2 elements these are subcommands
        subcommands_headings = [
            l.decode() for l in contents if l.startswith(b"## ")
        ]
        got_subcommands = [h[3:].strip() for h in subcommands_headings]
        got_subcommands.sort()
        self.assertEqual(want_subcommands, got_subcommands)


if __name__ == "__main__":
    unittest.main()
