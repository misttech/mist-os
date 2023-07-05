#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os

import unittest
from pathlib import Path
from typing import Iterable
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


class GCCSupportToolsTests(unittest.TestCase):

    def test_partial_paths_c(self):
        arch = 'x86_64'
        objfmt = 'elf'
        gcc_install_base = Path('../../some/where/install_gcc')
        version_dir = gcc_install_base / f'libexec/gcc/{arch}-{objfmt}/0.99'
        with mock.patch.object(Path, 'glob',
                               return_value=iter([version_dir
                                                 ])) as mock_version:
            tools = list(
                fuchsia.gcc_support_tools(
                    gcc_install_base / f'bin/{arch}-{objfmt}-gcc'))
        self.assertEqual({t.name for t in tools}, {'as', 'cc1', 'crtbegin.o'})
        # we only need crtbegin.o for the remote setup of its parent dir
        for t in tools:
            self.assertIn(gcc_install_base, t.parents)

    def test_partial_paths_cxx(self):
        arch = 'powerpc64'
        objfmt = 'macho'
        gcc_install_base = Path('../../some/where/install_gcc')
        version_dir = gcc_install_base / f'libexec/gcc/{arch}-{objfmt}/0.99'
        with mock.patch.object(Path, 'glob',
                               return_value=iter([version_dir
                                                 ])) as mock_version:
            tools = list(
                fuchsia.gcc_support_tools(
                    gcc_install_base / f'bin/{arch}-{objfmt}-g++'))
        self.assertEqual(
            {t.name for t in tools}, {'as', 'cc1plus', 'crtbegin.o'})
        # we only need crtbegin.o for the remote setup of its parent dir
        for t in tools:
            self.assertIn(gcc_install_base, t.parents)


class RustStdlibDirTests(unittest.TestCase):

    def test_substitution(self):
        target = 'powerpc-apple-darwin8'
        self.assertIn(target, fuchsia.rust_stdlib_subdir(target).parts)


class RustcTargetToSysrootTripleTests(unittest.TestCase):

    def test_known(self):
        for t in ('x86_64-linux-gnu', 'aarch64-linux-gnu', 'riscv64gc-fuchsia',
                  'x86_64-fuchsia'):
            fuchsia.rustc_target_to_sysroot_triple(t)

    def test_unknown(self):
        with self.assertRaises(ValueError):
            fuchsia.rustc_target_to_sysroot_triple('pdp11-alien-vax')


class RustcTargetToClangTargetTests(unittest.TestCase):

    def test_known(self):
        for t in ('x86_64-linux-gnu', 'aarch64-linux-gnu',
                  'riscv64gc-unknown-fuchsia', 'x86_64-unknown-fuchsia',
                  'x86_64-apple-darwin'):
            fuchsia.rustc_target_to_clang_target(t)

    def test_unknown(self):
        with self.assertRaises(ValueError):
            fuchsia.rustc_target_to_clang_target('pdp11-alien-vax')


def fake_linker_script_expander(path: Path) -> Iterable[Path]:
    yield path


class CSysrootFilesTest(unittest.TestCase):

    def test_list(self):
        with mock.patch.object(Path, 'is_file',
                               return_value=True) as mock_is_file:
            list(
                fuchsia.c_sysroot_files(
                    sysroot_dir=Path('path/to/built/sysroot'),
                    sysroot_triple='x86_64-linux-foo',
                    linker_script_expander=fake_linker_script_expander,
                    with_libgcc=True,
                ))
            list(
                fuchsia.c_sysroot_files(
                    sysroot_dir=Path('path/to/built/sysroot'),
                    sysroot_triple='riscv64-linux-foo',
                    linker_script_expander=fake_linker_script_expander,
                    with_libgcc=True,
                ))
            list(
                fuchsia.c_sysroot_files(
                    sysroot_dir=Path('path/to/built/sysroot'),
                    sysroot_triple='aarch64-linux-foo',
                    linker_script_expander=fake_linker_script_expander,
                    with_libgcc=True,
                ))


if __name__ == '__main__':
    unittest.main()
