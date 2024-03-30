#!/usr/bin/env fuchsia-vendored-python

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Helper methods for executing SDK developer workflow tests.
"""

import json
import os
import unittest
import argparse
import sys

from sdk_test_common import SDKCommands

sdk_id = ""


class FfxSdkVersionTest(unittest.TestCase):
    def test_ffx_sdk_version_returns_correct_value(self) -> None:
        commmands = SDKCommands()
        commmands.bootstrap()
        commmands.ffx("sdk version").capture()
        commmands.execute()

        global sdk_id
        expected = sdk_id
        actual = commmands.captured()
        self.assertEqual(expected, actual)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--test_dir", required=True)
    parser.add_argument("--sdk_id", required=True)
    args = parser.parse_args()

    if args.test_dir:
        os.chdir(args.test_dir)

    global sdk_id
    sdk_id = args.sdk_id

    unittest.main(argv=[sys.argv[0]])


if __name__ == "__main__":
    main()
