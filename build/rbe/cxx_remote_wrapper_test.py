#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import io
import os
import subprocess
import sys
import tempfile

from pathlib import Path
import unittest
from unittest import mock

import cxx_remote_wrapper

import cl_utils
import fuchsia
import cxx
import remote_action

from typing import Any, Sequence


class ImmediateExit(Exception):
    """For mocking functions that do not return."""
    pass


def _write_file_contents(path: Path, contents: str):
    with open(path, 'w') as f:
        f.write(contents)


def _read_file_contents(path: Path) -> str:
    with open(path, 'r') as f:
        return f.read()


def _strs(items: Sequence[Any]) -> Sequence[str]:
    return [str(i) for i in items]


def _paths(items: Sequence[Any]) -> Sequence[Path]:
    if isinstance(items, list):
        return [Path(i) for i in items]
    elif isinstance(items, set):
        return {Path(i) for i in items}
    elif isinstance(items, tuple):
        return tuple(Path(i) for i in items)

    t = type(items)
    raise TypeError(f"Unhandled sequence type: {t}")


class CheckMissingRemoteToolsTests(unittest.TestCase):

    _PROJECT_ROOT = Path('..', '..', 'fake', 'project', 'root')

    def test_clang_exists(self):
        with mock.patch.object(Path, 'is_dir', return_value=True):
            self.assertEqual(
                list(
                    cxx_remote_wrapper.check_missing_remote_tools(
                        cxx.Compiler.CLANG, self._PROJECT_ROOT)),
                [],
            )

    def test_clang_not_exists(self):
        with mock.patch.object(Path, 'is_dir', return_value=False):
            self.assertEqual(
                list(
                    cxx_remote_wrapper.check_missing_remote_tools(
                        cxx.Compiler.CLANG, self._PROJECT_ROOT)),
                [fuchsia.REMOTE_CLANG_SUBDIR],
            )

    def test_gcc_exists(self):
        with mock.patch.object(Path, 'is_dir', return_value=True):
            self.assertEqual(
                list(
                    cxx_remote_wrapper.check_missing_remote_tools(
                        cxx.Compiler.GCC, self._PROJECT_ROOT)),
                [],
            )

    def test_gcc_not_exists(self):
        with mock.patch.object(Path, 'is_dir', return_value=False):
            self.assertEqual(
                list(
                    cxx_remote_wrapper.check_missing_remote_tools(
                        cxx.Compiler.GCC, self._PROJECT_ROOT)),
                [fuchsia.REMOTE_GCC_SUBDIR],
            )


class CxxRemoteActionTests(unittest.TestCase):

    def test_clang_cxx(self):
        fake_root = Path('/home/project')
        fake_builddir = Path('out/really-not-default')
        fake_cwd = fake_root / fake_builddir
        compiler = Path('clang++')
        source = Path('hello.cc')
        output = Path('hello.o')
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21', '-c', source, '-o',
                output
            ])
        c = cxx_remote_wrapper.CxxRemoteAction(
            ['--'] + command,
            exec_root=fake_root,
            working_dir=fake_cwd,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.cxx_action.compiler.tool, compiler)
        self.assertTrue(c.cxx_action.compiler_is_clang)
        self.assertEqual(c.cxx_action.output_file, output)
        self.assertEqual(c.cxx_action.target, 'riscv64-apple-darwin21')
        self.assertEqual(c.cpp_strategy, 'integrated')
        self.assertEqual(c.original_compile_command, command)
        self.assertFalse(c.local_only)

        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            self.assertEqual(c.prepare(), 0)
        self.assertEqual(
            c.remote_action.inputs_relative_to_project_root,
            [fake_builddir / source])
        with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                               '_run_remote_action',
                               return_value=0) as mock_call:
            exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_once()

    def test_pp_asm_local_only(self):
        compiler = Path('clang++')
        source = Path('hello.S')
        output = Path('hello.o')
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21', '-c', source, '-o',
                output
            ])
        c = cxx_remote_wrapper.CxxRemoteAction(
            ['--'] + command,
            auto_reproxy=False,
        )
        self.assertTrue(c.local_only)
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(subprocess, 'call',
                                   return_value=0) as mock_call:
                exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_with(command)  # ran locally

    def test_objc_local_only(self):
        compiler = Path('clang++')
        source = Path('hello.mm')
        output = Path('hello.o')
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21', '-c', source, '-o',
                output
            ])
        c = cxx_remote_wrapper.CxxRemoteAction(
            ['--'] + command,
            auto_reproxy=False,
        )
        self.assertTrue(c.local_only)
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(subprocess, 'call',
                                   return_value=0) as mock_call:
                exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_with(command)  # ran locally

    def test_remote_action_paths(self):
        fake_root = Path('/home/project')
        fake_builddir = Path('out/not-default')
        fake_cwd = fake_root / fake_builddir
        compiler = Path('clang++')
        source = Path('hello.cc')
        output = Path('hello.o')
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21', '-c', source, '-o',
                output
            ])
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(os, 'curdir', fake_cwd):
                with mock.patch.object(remote_action, 'PROJECT_ROOT',
                                       fake_root):
                    c = cxx_remote_wrapper.CxxRemoteAction(
                        ['--'] + command,
                        auto_reproxy=False,
                    )
                    self.assertEqual(c.prepare(), 0)
                    self.assertEqual(c.remote_action.exec_root, fake_root)
                    self.assertEqual(
                        c.remote_action.build_subdir, fake_builddir)

    def test_clang_crash_diagnostics_dir(self):
        fake_root = Path('/usr/project')
        fake_builddir = Path('build-it')
        fake_cwd = fake_root / fake_builddir
        crash_dir = Path('boom/b00m')
        compiler = Path('clang++')
        source = Path('hello.cc')
        output = Path('hello.o')
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21',
                f'-fcrash-diagnostics-dir={crash_dir}', '-c', source, '-o',
                output
            ])
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(remote_action, 'PROJECT_ROOT', fake_root):
                c = cxx_remote_wrapper.CxxRemoteAction(
                    ['--'] + command, working_dir=fake_cwd, auto_reproxy=False)
                self.assertEqual(c.prepare(), 0)
                self.assertTrue(c.cxx_action.compiler_is_clang)
                self.assertEqual(c.remote_action.exec_root, fake_root)
                self.assertEqual(c.remote_action.build_subdir, fake_builddir)
                self.assertEqual(
                    c.remote_action.output_dirs_relative_to_working_dir,
                    [crash_dir])
                self.assertEqual(
                    c.remote_action.output_dirs_relative_to_project_root,
                    [fake_builddir / crash_dir])

    def test_remote_flag_back_propagating(self):
        compiler = Path('clang++')
        source = Path('hello.cc')
        output = Path('hello.o')
        flag = '--foo-bar'
        command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21',
                f'--remote-flag={flag}', '-c', source, '-o', output
            ])
        filtered_command = _strs(
            [
                compiler, '--target=riscv64-apple-darwin21', '-c', source, '-o',
                output
            ])

        c = cxx_remote_wrapper.CxxRemoteAction(
            ['--'] + command,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )

        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            self.assertEqual(c.prepare(), 0)
        # check that rewrapper option sees --foo=bar
        remote_action_command = c.remote_action.launch_command
        prefix, sep, wrapped_command = cl_utils.partition_sequence(
            remote_action_command, '--')
        self.assertIn(flag, prefix)
        self.assertEqual(wrapped_command, filtered_command)

    def test_gcc_cxx(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = Path('make-it-so')
        fake_cwd = fake_root / fake_builddir
        compiler = Path('g++')
        source = Path('hello.cc')
        output = Path('hello.o')
        depfile = Path('hello.d')
        command = _strs([compiler, '-MF', depfile, '-c', source, '-o', output])
        c = cxx_remote_wrapper.CxxRemoteAction(
            ['--'] + command,
            working_dir=fake_cwd,
            exec_root=fake_root,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.cxx_action.compiler.tool, compiler)
        self.assertTrue(c.cxx_action.compiler_is_gcc)
        self.assertEqual(c.cxx_action.output_file, output)
        self.assertEqual(c.cpp_strategy, 'integrated')
        self.assertEqual(c.original_compile_command, command)
        self.assertFalse(c.local_only)
        self.assertEqual(c.depfile, depfile)

        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(fuchsia, 'gcc_support_tools',
                                   return_value=iter([])) as mock_tools:
                self.assertEqual(c.prepare(), 0)
        mock_tools.assert_called_with(c.compiler_path)
        self.assertEqual(
            c.remote_action.inputs_relative_to_project_root,
            [fake_builddir / source])
        self.assertEqual(
            c.remote_action.output_files_relative_to_project_root,
            [fake_builddir / output])
        self.assertEqual(set(c.remote_action.always_download), set([depfile]))

        with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                               '_rewrite_remote_depfile') as mock_rewrite:
            with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                                   '_depfile_exists',
                                   return_value=True) as mock_exists:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_run_maybe_remotely',
                                       return_value=cl_utils.SubprocessResult(
                                           0)) as mock_remote:
                    exit_code = c.run()

        self.assertEqual(exit_code, 0)
        mock_rewrite.assert_called_with()
        mock_exists.assert_called_with()

    def test_rewrite_remote_depfile(self):
        compiler = Path('ppc-macho-g++')
        source = Path('hello.cc')
        output = Path('hello.o')
        depfile = Path('hello.d')
        with tempfile.TemporaryDirectory() as td:
            fake_root = Path(td)
            fake_builddir = Path('make-it-so')
            fake_cwd = fake_root / fake_builddir
            command = _strs(
                [compiler, '-MF', depfile, '-c', source, '-o', output])
            c = cxx_remote_wrapper.CxxRemoteAction(
                # For this test, make the remote/local working dirs match
                ['--canonicalize_working_dir=false', '--'] + command,
                working_dir=fake_cwd,
                exec_root=fake_root,
                host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
                auto_reproxy=False,
            )

            remote_cwd = remote_action._REMOTE_PROJECT_ROOT / fake_builddir
            fake_cwd.mkdir(parents=True, exist_ok=True)
            _write_file_contents(
                fake_cwd / depfile,
                f'{remote_cwd}/lib/bar.a: {remote_cwd}/obj/foo.o\n')

            with mock.patch.object(cxx_remote_wrapper,
                                   'check_missing_remote_tools') as mock_check:
                # create the remote action
                self.assertEqual(c.prepare(), 0)

            mock_check.asert_called_once()
            self.assertEqual(
                set(c.remote_action.always_download), set([depfile]))
            self.assertEqual(c.remote_action.remote_working_dir, remote_cwd)
            c._rewrite_remote_depfile()
            new_depfile = _read_file_contents(fake_cwd / depfile)
            self.assertEqual(new_depfile, 'lib/bar.a: obj/foo.o\n')

    def test_remote_cross_compile_clang_with_integrated_preprocessing(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = Path('make-it-so')
        fake_cwd = fake_root / fake_builddir
        compiler_relpath = Path('../path/to/clang/mac-arm64/clang++')
        remote_compiler_relpath = Path(
            '../path/to/clang', fuchsia.REMOTE_PLATFORM, 'clang++')
        compiler_swapper = Path('scripts/swapperoo.sh')
        source = Path('hello.cc')
        output = Path('hello.o')
        command = _strs(
            [
                compiler_relpath, '--target=riscv64-apple-darwin21', '-c',
                source, '-o', output
            ])
        with mock.patch.object(cxx_remote_wrapper, 'REMOTE_COMPILER_SWAPPER',
                               fake_root / compiler_swapper):
            c = cxx_remote_wrapper.CxxRemoteAction(
                [f'--exec_root={fake_root}', '--'] + command,
                working_dir=fake_cwd,
                exec_root=fake_root,
                # Pretend host != 'linux-x64' to cross-compile
                host_platform='mac-arm64',
                auto_reproxy=False,
            )
            # compile command doesn't reference Mac SDK, so remote integrated
            # preprocessing is possible.
            self.assertEqual(c.cpp_strategy, 'integrated')
            with mock.patch.object(cxx_remote_wrapper,
                                   'check_missing_remote_tools') as mock_check:
                self.assertEqual(c.prepare(), 0)

            self.assertEqual(c.remote_compiler, remote_compiler_relpath)
            self.assertEqual(
                set(c.remote_action.inputs_relative_to_project_root), {
                    fake_builddir / source,
                    Path('path/to/clang/linux-x64/clang++'), compiler_swapper
                })
            remote_compile_command = c.remote_action.launch_command
            rewrapper_prefix, sep, wrapped_command = cl_utils.partition_sequence(
                remote_compile_command, '--')
            self.assertIn(
                f'--remote_wrapper=../{compiler_swapper}', rewrapper_prefix)
            self.assertEqual(wrapped_command, command)

        # make sure preprocessing status is propagated (if it is run)
        for status in (0, 1):
            with mock.patch.object(subprocess, 'call',
                                   return_value=status) as mock_cpp:
                cpp_status = c.preprocess_locally()
            self.assertEqual(cpp_status, status)
            mock_cpp.assert_called_once()

        # preprocessing integrated into remote cross compile
        remote_status = 3
        with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                               'preprocess_locally') as mock_cpp:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=remote_status) as mock_remote:
                run_status = c.run()
        self.assertEqual(run_status, remote_status)
        mock_cpp.assert_not_called()
        mock_remote.assert_called_once()

    def test_remote_cross_compile_clang_with_local_preprocessing(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = Path('make-it-so')
        fake_cwd = fake_root / fake_builddir
        compiler_relpath = Path('../path/to/clang/mac-arm64/clang++')
        remote_compiler_relpath = Path(
            '../path/to/clang', fuchsia.REMOTE_PLATFORM, 'clang++')
        compiler_swapper = Path('scripts/swapperoo.sh')
        source = Path('hello.cc')
        output = Path('hello.o')
        command = _strs(
            [
                compiler_relpath, '--target=riscv64-apple-darwin21', '-c',
                source, '-o', output,
                '--sysroot=/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk'
            ])
        with mock.patch.object(cxx_remote_wrapper, 'REMOTE_COMPILER_SWAPPER',
                               fake_root / compiler_swapper):
            c = cxx_remote_wrapper.CxxRemoteAction(
                [f'--exec_root={fake_root}', '--'] + command,
                working_dir=fake_cwd,
                exec_root=fake_root,
                # Pretend host != 'linux-x64' to cross-compile
                host_platform='mac-arm64',
                auto_reproxy=False,
            )
            # compile command references Mac SDK, so preprocess locally
            self.assertEqual(c.cpp_strategy, 'local')
            with mock.patch.object(cxx_remote_wrapper,
                                   'check_missing_remote_tools') as mock_check:
                with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                                       'preprocess_locally',
                                       return_value=0) as mock_cpp:
                    self.assertEqual(c.prepare(), 0)
                mock_cpp.assert_called_once()

            self.assertEqual(c.remote_compiler, remote_compiler_relpath)
            self.assertEqual(
                set(c.remote_action.inputs_relative_to_project_root), {
                    fake_builddir / source,
                    Path('path/to/clang/linux-x64/clang++'), compiler_swapper,
                    fake_builddir / c.cxx_action.preprocessed_output
                })
            remote_compile_command = c.remote_action.launch_command
            rewrapper_prefix, sep, wrapped_command = cl_utils.partition_sequence(
                remote_compile_command, '--')
            self.assertIn(
                f'--remote_wrapper=../{compiler_swapper}', rewrapper_prefix)
            self.assertIn(
                str(c.cxx_action.preprocessed_output), wrapped_command)

        # make sure preprocessing status is propagated (if it is run)
        for status in (0, 1):
            with mock.patch.object(subprocess, 'call',
                                   return_value=status) as mock_cpp:
                cpp_status = c.preprocess_locally()
            self.assertEqual(cpp_status, status)
            mock_cpp.assert_called_once()

        # remote cross compile
        remote_status = 4
        with mock.patch.object(remote_action.RemoteAction, 'run_with_main_args',
                               return_value=remote_status) as mock_remote:
            with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                                   '_cleanup') as mock_cleanup:
                run_status = c.run()
        self.assertEqual(run_status, remote_status)
        mock_remote.assert_called_once()
        mock_cleanup.assert_called_with()

    def test_remote_cross_compile_gcc_integrated_preprocessing(self):
        fake_root = remote_action.PROJECT_ROOT
        fake_builddir = Path('make-it-so')
        fake_cwd = fake_root / fake_builddir
        root_rel = cl_utils.relpath(fake_root, start=fake_cwd)
        compiler_relpath = Path('../path/to/gcc/mac-arm64/g++')
        remote_compiler_relpath = Path(
            '../path/to/gcc', fuchsia.REMOTE_PLATFORM, 'g++')
        compiler_swapper = Path('scripts/swapperoo.sh')
        source_in_root = Path('src/hello.cc')
        source = root_rel / source_in_root
        output = Path('hello.o')
        command = _strs(
            [
                compiler_relpath, '--target=riscv64-apple-darwin21', '-c',
                source, '-o', output
            ])
        with mock.patch.object(cxx_remote_wrapper,
                               'check_missing_remote_tools') as mock_check:
            with mock.patch.object(cxx_remote_wrapper,
                                   'REMOTE_COMPILER_SWAPPER',
                                   fake_root / compiler_swapper):
                c = cxx_remote_wrapper.CxxRemoteAction(
                    [f'--exec_root={fake_root}', '--'] + command,
                    working_dir=fake_cwd,
                    exec_root=fake_root,
                    # Pretend host != 'linux-x64' to cross-compile
                    host_platform='mac-arm64',
                    auto_reproxy=False,
                )
                self.assertEqual(c.cpp_strategy, 'integrated')
                with mock.patch.object(fuchsia, 'gcc_support_tools',
                                       return_value=iter([])) as mock_tools:
                    self.assertEqual(c.prepare(), 0)
                mock_tools.assert_called_with(c.compiler_path)
                self.assertEqual(c.remote_compiler, remote_compiler_relpath)
                self.assertEqual(
                    set(c.remote_action.inputs_relative_to_project_root), {
                        source_in_root,
                        Path('path/to/gcc/linux-x64/g++'), compiler_swapper
                    })
                remote_compile_command = c.remote_action.launch_command
                rewrapper_prefix, sep, wrapped_command = cl_utils.partition_sequence(
                    remote_compile_command, '--')
                self.assertIn(
                    f'--remote_wrapper=../{compiler_swapper}', rewrapper_prefix)
                self.assertEqual(wrapped_command, command)


class MainTests(unittest.TestCase):

    def test_help_implicit(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    cxx_remote_wrapper.main([])
        mock_exit.assert_called_with(0)

    def test_help_flag(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    cxx_remote_wrapper.main(['--help'])
        mock_exit.assert_called_with(0)

    def test_local_mode_forced(self):
        exit_code = 24
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(cxx_remote_wrapper.CxxRemoteAction,
                                   '_run_locally',
                                   return_value=exit_code) as mock_run:
                self.assertEqual(
                    cxx_remote_wrapper.main(
                        [
                            '--local', '--', 'clang++', '-c', 'foo.cc', '-o',
                            'foo.o'
                        ]), exit_code)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_with()

    def test_auto_relaunched_with_reproxy(self):
        argv = ['--', 'clang++', '-c', 'foo.cc', '-o', 'foo.o']
        with mock.patch.object(os.environ, 'get',
                               return_value=None) as mock_env:
            with mock.patch.object(cl_utils, 'exec_relaunch',
                                   side_effect=ImmediateExit) as mock_relaunch:
                with self.assertRaises(ImmediateExit):
                    cxx_remote_wrapper.main(argv)
        mock_env.assert_called()
        mock_relaunch.assert_called_once()
        args, kwargs = mock_relaunch.call_args_list[0]
        relaunch_cmd = args[0]
        self.assertEqual(relaunch_cmd[0], fuchsia.REPROXY_WRAP)
        cmd_slices = cl_utils.split_into_subsequences(relaunch_cmd[1:], '--')
        reproxy_args, self_script, wrapped_command = cmd_slices
        self.assertEqual(reproxy_args, [])
        self.assertIn('python', self_script[0])
        self.assertTrue(self_script[-1].endswith('cxx_remote_wrapper.py'))
        self.assertEqual(wrapped_command, argv[1:])


if __name__ == '__main__':
    unittest.main()
