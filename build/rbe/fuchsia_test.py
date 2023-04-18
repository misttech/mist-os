#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os

import unittest
from pathlib import Path
from unittest import mock

import fuchsia

# Most tests here just make sure there are no Python syntax or typing errors.
# There is no need to test change-detection of constant values.


class RemoteExecutableTests(unittest.TestCase):

    def test_linux_x64(self):
        self.assertEqual(
            fuchsia.remote_executable(
                Path('../prebuilt/some/tool/linux-x64/bin/tool1')),
            Path('../prebuilt/some/tool/linux-x64/bin/tool1'),
        )

    def test_mac_arm64(self):
        self.assertEqual(
            fuchsia.remote_executable(
                Path('../prebuilt/some/tool/mac-arm64/bin/tool2')),
            Path('../prebuilt/some/tool/linux-x64/bin/tool2'),
        )


class RustStdlibDirTests(unittest.TestCase):

    def test_substitution(self):
        target = 'powerpc-apple-darwin8'
        self.assertIn(target, fuchsia.rust_stdlib_dir(target).parts)


class RustcTargetToSysrootTripleTests(unittest.TestCase):

    def test_known(self):
        for t in ('x86_64-linux-gnu', 'aarch64-linux-gnu', 'x86_64-fuchsia'):
            fuchsia.rustc_target_to_sysroot_triple(t)

    def test_unknown(self):
        with self.assertRaises(ValueError):
            fuchsia.rustc_target_to_sysroot_triple('pdp11-alien-vax')


class RustcTargetToClangTargetTests(unittest.TestCase):

    def test_known(self):
        for t in ('x86_64-linux-gnu', 'aarch64-linux-gnu',
                  'x86_64-unknown-fuchsia'):
            fuchsia.rustc_target_to_clang_target(t)

    def test_unknown(self):
        with self.assertRaises(ValueError):
            fuchsia.rustc_target_to_clang_target('pdp11-alien-vax')


class SysrootFilesTest(unittest.TestCase):

    def test_list(self):
        with mock.patch.object(Path, 'is_file',
                               return_value=True) as mock_is_file:
            list(
                fuchsia.sysroot_files(
                    sysroot_dir=Path('path/to/built/sysroot'),
                    sysroot_triple='x86_64-linux-foo',
                    with_libgcc=True,
                ))
            list(
                fuchsia.sysroot_files(
                    sysroot_dir=Path('path/to/built/sysroot'),
                    sysroot_triple='aarch64-linux-foo',
                    with_libgcc=True,
                ))


if __name__ == '__main__':
    unittest.main()
