#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""adb end to end test scripts."""

import argparse
import os
import subprocess
import sys
import unittest

parser = argparse.ArgumentParser()
parser.add_argument('--adb_path', help='Path to the adb binary.', required=True)
ARGS, UT_ARGV = parser.parse_known_args(sys.argv)


class AdbTest(unittest.TestCase):

    def setUp(self):
        self.adb_binary = os.path.join(ARGS.adb_path, 'adb')

    def _run_adb(self, *args: str) -> str:
        """Run adb binary with argv constructed from the given args."""
        argv = [self.adb_binary, *args]
        return subprocess.check_output(
            argv, stderr=subprocess.STDOUT, text=True)

    def test_list_devices(self):
        output = self._run_adb('devices', '-l')
        self.assertIn('device:zircon', output)

    def test_adb_shell(self):
        output = self._run_adb('shell', 'echo \"hello\"')
        self.assertEqual('hello\n', output)

    def test_adb_reboot(self):
        output = self._run_adb('reboot')
        self.assertEqual('', output)

        output = self._run_adb('wait-for-disconnect')
        self.assertEqual('', output)

        output = self._run_adb('wait-for-device')
        self.assertEqual('', output)


def main():
    unittest.main(argv=UT_ARGV)


if __name__ == '__main__':
    main()
