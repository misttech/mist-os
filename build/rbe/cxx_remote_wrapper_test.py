#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import subprocess

import unittest
from unittest import mock

import cxx_remote_wrapper

import fuchsia
import remote_action


class CxxRemoteActionTests(unittest.TestCase):

    @mock.patch.object(fuchsia, 'HOST_PREBUILT_PLATFORM_SUBDIR', 'linux-x64')
    def test_clang_cxx(self):
        command = [
            'clang++', '--target=riscv64-apple-darwin21', '-c', 'hello.cc',
            '-o', 'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            c = cxx_remote_wrapper.CxxRemoteAction(['--'] + command)
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.cxx_action.compiler.tool, 'clang++')
        self.assertTrue(c.cxx_action.compiler_is_clang)
        self.assertEqual(c.cxx_action.output_file, 'hello.o')
        self.assertEqual(c.cxx_action.target, 'riscv64-apple-darwin21')
        self.assertEqual(c.cpp_strategy, 'integrated')
        self.assertEqual(c.original_compile_command, command)
        self.assertFalse(c.local_only)
        # no additional inputs, hello.cc is already implicitly included
        self.assertEqual(
            c.remote_compile_action.inputs_relative_to_project_root, [])
        with mock.patch.object(subprocess, 'call', return_value=0) as mock_call:
            exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_once()

    def test_pp_asm_local_only(self):
        command = [
            'clang++', '--target=riscv64-apple-darwin21', '-c', 'hello.S', '-o',
            'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            c = cxx_remote_wrapper.CxxRemoteAction(['--'] + command)
        self.assertTrue(c.local_only)
        with mock.patch.object(subprocess, 'call', return_value=0) as mock_call:
            exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_with(command)  # ran locally

    def test_remote_action_paths(self):
        fake_root = '/home/project'
        fake_builddir = 'out/not-default'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        command = [
            'clang++', '--target=riscv64-apple-darwin21', '-c', 'hello.cc',
            '-o', 'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(os, 'curdir', fake_cwd):
                with mock.patch.object(remote_action, 'PROJECT_ROOT',
                                       fake_root):
                    c = cxx_remote_wrapper.CxxRemoteAction(['--'] + command)
                    self.assertEqual(
                        c.remote_compile_action.exec_root, fake_root)
                    self.assertEqual(
                        c.remote_compile_action.build_subdir, fake_builddir)

    def test_clang_crash_diagnostics_dir(self):
        fake_root = '/usr/project'
        fake_builddir = 'build-it'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        command = [
            'clang++', '--target=riscv64-apple-darwin21',
            '-fcrash-diagnostics-dir=boom/boom', '-c', 'hello.cc', '-o',
            'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(remote_action, 'PROJECT_ROOT', fake_root):
                c = cxx_remote_wrapper.CxxRemoteAction(
                    ['--'] + command, working_dir=fake_cwd)
                self.assertTrue(c.cxx_action.compiler_is_clang)
                self.assertEqual(c.remote_compile_action.exec_root, fake_root)
                self.assertEqual(
                    c.remote_compile_action.build_subdir, fake_builddir)
                self.assertEqual(
                    c.remote_compile_action.
                    output_dirs_relative_to_project_root,
                    [os.path.join(fake_builddir, 'boom/boom')])

    @mock.patch.object(fuchsia, 'HOST_PREBUILT_PLATFORM_SUBDIR', 'linux-x64')
    def test_remote_flag_back_propagating(self):
        command = [
            'clang++', '--target=riscv64-apple-darwin21',
            '--remote-flag=--foo=bar', '-c', 'hello.cc', '-o', 'hello.o'
        ]
        filtered_command = [
            'clang++', '--target=riscv64-apple-darwin21', '-c', 'hello.cc',
            '-o', 'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            c = cxx_remote_wrapper.CxxRemoteAction(['--'] + command)
        # check that rewrapper option sees --foo=bar
        remote_action_command = c.remote_compile_action.command
        ddash = remote_action_command.index('--')
        self.assertIn('--foo=bar', remote_action_command[:ddash])
        self.assertEqual(remote_action_command[ddash + 1:], filtered_command)

    @mock.patch.object(fuchsia, 'HOST_PREBUILT_PLATFORM_SUBDIR', 'linux-x64')
    def test_gcc_cxx(self):
        command = ['g++', '-c', 'hello.cc', '-o', 'hello.o']
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            c = cxx_remote_wrapper.CxxRemoteAction(['--'] + command)
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.cxx_action.compiler.tool, 'g++')
        self.assertTrue(c.cxx_action.compiler_is_gcc)
        self.assertEqual(c.cxx_action.output_file, 'hello.o')
        self.assertEqual(c.cpp_strategy, 'integrated')
        self.assertEqual(c.original_compile_command, command)
        self.assertFalse(c.local_only)
        # no additional inputs, hello.cc is already implicitly included
        self.assertEqual(
            c.remote_compile_action.inputs_relative_to_project_root, [])
        with mock.patch.object(subprocess, 'call', return_value=0) as mock_call:
            exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_once()

    # Pretend host != 'linux-x64'
    @mock.patch.object(fuchsia, 'HOST_PREBUILT_PLATFORM_SUBDIR', 'mac-arm64')
    def test_remote_cross_compile_clang(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = 'make-it-so'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        compiler_relpath = '../path/to/clang/mac-arm64/clang++'
        remote_compiler_relpath = os.path.join(
            '../path/to/clang', fuchsia.REMOTE_PLATFORM_SUBDIR, 'clang++')
        command = [
            compiler_relpath, '--target=riscv64-apple-darwin21', '-c',
            'hello.cc', '-o', 'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(cxx_remote_wrapper,
                                   'REMOTE_COMPILER_SWAPPER',
                                   os.path.join(fake_root,
                                                'scripts/swapperoo.sh')):
                c = cxx_remote_wrapper.CxxRemoteAction(
                    [f'--exec_root={fake_root}', '--'] + command,
                    working_dir=fake_cwd)
                self.assertEqual(c.remote_compiler, remote_compiler_relpath)
                self.assertEqual(
                    c.remote_compile_action.inputs_relative_to_project_root,
                    ['path/to/clang/linux-x64/clang++'])
                remote_compile_command = c.remote_compile_action.command
                ddash = remote_compile_command.index('--')
                rewrapper_prefix = remote_compile_command[:ddash]
                self.assertIn(
                    '--remote_wrapper=../scripts/swapperoo.sh',
                    rewrapper_prefix)

    # Pretend host != 'linux-x64'
    @mock.patch.object(fuchsia, 'HOST_PREBUILT_PLATFORM_SUBDIR', 'mac-arm64')
    def test_remote_cross_compile_gcc(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = 'make-it-so'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        compiler_relpath = '../path/to/gcc/mac-arm64/g++'
        remote_compiler_relpath = os.path.join(
            '../path/to/gcc', fuchsia.REMOTE_PLATFORM_SUBDIR, 'g++')
        command = [
            compiler_relpath, '--target=riscv64-apple-darwin21', '-c',
            'hello.cc', '-o', 'hello.o'
        ]
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(cxx_remote_wrapper,
                                   'REMOTE_COMPILER_SWAPPER',
                                   os.path.join(fake_root,
                                                'scripts/swapperoo.sh')):
                c = cxx_remote_wrapper.CxxRemoteAction(
                    [f'--exec_root={fake_root}', '--'] + command,
                    working_dir=fake_cwd)
                self.assertEqual(c.remote_compiler, remote_compiler_relpath)
                self.assertEqual(
                    c.remote_compile_action.inputs_relative_to_project_root,
                    ['path/to/gcc/linux-x64/g++'])
                remote_compile_command = c.remote_compile_action.command
                ddash = remote_compile_command.index('--')
                rewrapper_prefix = remote_compile_command[:ddash]
                self.assertIn(
                    '--remote_wrapper=../scripts/swapperoo.sh',
                    rewrapper_prefix)


if __name__ == '__main__':
    unittest.main()
