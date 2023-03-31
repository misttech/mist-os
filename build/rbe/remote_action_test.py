#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import subprocess
import unittest
from unittest import mock

import fuchsia
import remote_action
import cl_utils


class ReclientCanonicalWorkingDirTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(remote_action.reclient_canonical_working_dir(''), '')

    def test_one_level(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir('build-here'),
            'set_by_reclient')

    def test_two_levels(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir('build/there'),
            'set_by_reclient/a')

    def test_three_levels(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir('build/inside/there'),
            'set_by_reclient/a/a')


class RemoveWorkingDirAbspathsTests(unittest.TestCase):

    def test_blank_no_change(self):
        for t in ('', '\n'):
            self.assertEqual(
                remote_action.remove_working_dir_abspaths(t, 'build-out'), t)

    def test_phony_dep_with_local_build_dir(self):
        self.assertEqual(
            remote_action.remove_working_dir_abspaths(
                f'{remote_action._REMOTE_PROJECT_ROOT}/out/foo/path/to/thing.txt:\n',
                'out/foo'),
            'path/to/thing.txt:\n',
        )

    def test_phony_dep_with_canonical_build_dir(self):
        self.assertEqual(
            remote_action.remove_working_dir_abspaths(
                f'{remote_action._REMOTE_PROJECT_ROOT}/set_by_reclient/a/path/to/somethingelse.txt:\n',
                'out/foo'),
            'path/to/somethingelse.txt:\n',
        )

    def test_typical_dep_line_multiple_occurrences(self):
        root = remote_action._REMOTE_PROJECT_ROOT
        subdir = 'out/inside/here'
        wd = os.path.join(root, subdir)
        self.assertEqual(
            remote_action.remove_working_dir_abspaths(
                f'obj/foo.o: {wd}/foo/bar.h {wd}/baz/quux.h\n', subdir),
            'obj/foo.o: foo/bar.h baz/quux.h\n',
        )


class ResolvedShlibsFromLddTests(unittest.TestCase):

    def test_sample(self):
        ldd_output = """
	linux-vdso.so.1 (0x00007ffd653b2000)
	librustc_driver-897e90da9cc472c4.so => /usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so (0x00007f6fdf600000)
	libstd-374958b5d3497a8f.so => /usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so (0x00007f6fdf45c000)
	libdl.so.2 => /lib/x86_64-linux-gnu/libdl.so.2 (0x00007f6fe2cc6000)
	librt.so.1 => /lib/x86_64-linux-gnu/librt.so.1 (0x00007f6fe2cc1000)
	libpthread.so.0 => /lib/x86_64-linux-gnu/libpthread.so.0 (0x00007f6fe2cba000)
	libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f6fdf27b000)
	libLLVM-15-rust-1.70.0-nightly.so => /usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so (0x00007f6fdb000000)
	libm.so.6 => /lib/x86_64-linux-gnu/libm.so.6 (0x00007f6fe2921000)
	/lib64/ld-linux-x86-64.so.2 (0x00007f6fe2ce6000)
"""
        self.assertEqual(
            list(
                remote_action.resolved_shlibs_from_ldd(
                    ldd_output.splitlines())),
            [
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
                '/lib/x86_64-linux-gnu/libdl.so.2',
                '/lib/x86_64-linux-gnu/librt.so.1',
                '/lib/x86_64-linux-gnu/libpthread.so.0',
                '/lib/x86_64-linux-gnu/libc.so.6',
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
                '/lib/x86_64-linux-gnu/libm.so.6',
            ])


class HostToolNonsystemShlibsTests(unittest.TestCase):

    def test_sample(self):
        unfiltered_shlibs = [
            '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
            '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
            '/lib/x86_64-linux-gnu/libdl.so.2',
            '/lib/x86_64-linux-gnu/librt.so.1',
            '/lib/x86_64-linux-gnu/libpthread.so.0',
            '/lib/x86_64-linux-gnu/libc.so.6',
            '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
            '/lib/x86_64-linux-gnu/libm.so.6',
            '/usr/lib/something_else.so',
        ]
        with mock.patch.object(
                remote_action, 'host_tool_shlibs',
                return_value=unfiltered_shlibs) as mock_host_tool_shlibs:
            self.assertEqual(
                list(
                    remote_action.host_tool_nonsystem_shlibs(
                        '../path/to/rustc')),
                [
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
                ])
        mock_host_tool_shlibs.assert_called_once()


class RewrapperArgParserTests(unittest.TestCase):

    def test_exec_root(self):
        args, _ = remote_action._REWRAPPER_ARG_PARSER.parse_known_args(
            ['--exec_root=/foo/bar'])
        self.assertEqual(args.exec_root, '/foo/bar')


class RemoteActionMainParserTests(unittest.TestCase):

    def _make_main_parser(self) -> argparse.ArgumentParser:
        parser = argparse.ArgumentParser()
        remote_action.inherit_main_arg_parser_flags(
            parser,
            default_cfg='default.cfg',
            default_bindir='/opt/reclient/bin')
        return parser

    def test_defaults(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--', 'echo', 'hello'])
        self.assertEqual(main_args.cfg, 'default.cfg')
        self.assertEqual(main_args.bindir, '/opt/reclient/bin')
        self.assertFalse(main_args.dry_run)
        self.assertFalse(main_args.verbose)
        self.assertEqual(main_args.label, '')
        self.assertEqual(main_args.remote_log, '')
        self.assertFalse(main_args.save_temps)
        self.assertFalse(main_args.auto_reproxy)
        self.assertEqual(main_args.command, ['echo', 'hello'])

    def test_cfg(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--cfg=other.cfg', '--', 'echo'])
        self.assertEqual(main_args.cfg, 'other.cfg')
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_command, ['echo'])
        self.assertEqual(action.options, ['--cfg', 'other.cfg'])

    def test_bindir(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--bindir', '/usr/local/bin', '--', 'echo'])
        self.assertEqual(main_args.bindir, '/usr/local/bin')
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_command, ['echo'])

    def test_local_command_with_env(self):
        local_command = (['FOO=BAR', 'echo'])
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--'] + local_command)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(
            action.local_command, [remote_action._ENV] + local_command)

    def test_verbose(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--verbose', '--', 'echo'])
        self.assertTrue(main_args.verbose)

    def test_dry_run(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--dry-run', '--', 'echo'])
        self.assertTrue(main_args.dry_run)
        action = remote_action.remote_action_from_args(main_args)
        with mock.patch.object(remote_action.RemoteAction, 'run') as mock_run:
            exit_code = action.run_with_main_args(main_args)
        self.assertEqual(exit_code, 0)
        mock_run.assert_not_called()

    @mock.patch.object(fuchsia, 'REPROXY_WRAP', '/path/to/reproxy-wrap.sh')
    def test_auto_reproxy(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--auto-reproxy', '--', 'echo'])
        self.assertTrue(main_args.auto_reproxy)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_command, ['echo'])
        self.assertEqual(action.command[:2], ['/path/to/reproxy-wrap.sh', '--'])

    def test_save_temps(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--save-temps', '--', 'echo'])
        self.assertTrue(main_args.save_temps)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_command, ['echo'])
        self.assertTrue(action.save_temps)

    def test_label(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--label=//build/this:that', '--', 'echo'])
        self.assertEqual(main_args.label, '//build/this:that')

    def test_remote_log(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--log', 'bar.remote-log', '--', 'echo'])
        self.assertEqual(main_args.remote_log, 'bar.remote-log')


class RemoteActionFlagParserTests(unittest.TestCase):

    def test_defaults(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args([])
        self.assertFalse(remote_args.disable)
        self.assertEqual(remote_args.inputs, [])
        self.assertEqual(remote_args.output_files, [])
        self.assertEqual(remote_args.output_dirs, [])
        self.assertEqual(remote_args.flags, [])
        self.assertEqual(other, [])

    def test_command_without_forwarding(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        command = [
            'clang++', '--target=powerpc-apple-darwin8',
            '-fcrash-diagnostics-dir=nothing/to/see/here', '-c', 'hello.cxx',
            '-o', 'hello.o'
        ]
        remote_args, other = p.parse_known_args(command)
        self.assertFalse(remote_args.disable)
        self.assertEqual(remote_args.inputs, [])
        self.assertEqual(remote_args.output_files, [])
        self.assertEqual(remote_args.output_dirs, [])
        self.assertEqual(remote_args.flags, [])
        self.assertEqual(other, command)

    def test_disable(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            ['cat', 'foo.txt', '--remote-disable'])
        self.assertTrue(remote_args.disable)
        self.assertEqual(other, ['cat', 'foo.txt'])

    def test_inputs(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            [
                'cat', '--remote-inputs=bar.txt', 'bar.txt',
                '--remote-inputs=quux.txt', 'quux.txt'
            ])
        self.assertEqual(remote_args.inputs, ['bar.txt', 'quux.txt'])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])

    def test_inputs_comma(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            [
                'cat', '--remote-inputs=w,x', 'bar.txt', '--remote-inputs=y,z',
                'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.inputs)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])

    def test_output_files_comma(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            [
                './generate.sh', '--remote-outputs=w,x', 'bar.txt',
                '--remote-outputs=y,z', 'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.output_files)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(other, ['./generate.sh', 'bar.txt', 'quux.txt'])

    def test_output_dirs_comma(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            [
                './generate_dirs.sh', '--remote-output-dirs=w,x', 'bar.txt',
                '--remote-output-dirs=y,z', 'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.output_dirs)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(other, ['./generate_dirs.sh', 'bar.txt', 'quux.txt'])

    def test_flags(self):
        p = remote_action.REMOTE_FLAG_ARG_PARSER
        remote_args, other = p.parse_known_args(
            [
                'cat', '--remote-flag=--foo=bar', 'bar.txt',
                '--remote-flag=--opt=quux', 'quux.txt'
            ])
        self.assertEqual(remote_args.flags, ['--foo=bar', '--opt=quux'])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])


class RemoteActionConstructionTests(unittest.TestCase):

    _PROJECT_ROOT = '/my/project/root'
    _WORKING_DIR = os.path.join(_PROJECT_ROOT, 'build_dir')

    def test_minimal(self):
        rewrapper = '/path/to/rewrapper'
        command = ['cat', 'meow.txt']
        action = remote_action.RemoteAction(
            rewrapper=rewrapper,
            command=command,
            exec_root=self._PROJECT_ROOT,
            working_dir=self._WORKING_DIR,
        )
        self.assertEqual(action.local_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)
        self.assertFalse(action.save_temps)
        self.assertFalse(action.auto_reproxy)
        self.assertFalse(action.remote_disable)
        self.assertEqual(action.build_subdir, 'build_dir')
        self.assertEqual(
            action.command,
            [rewrapper, f'--exec_root={self._PROJECT_ROOT}', '--'] + command)

    def test_path_setup_implicit(self):
        command = ['beep', 'boop']
        fake_root = '/home/project'
        fake_builddir = 'out/not-default'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        with mock.patch.object(os, 'curdir', fake_cwd):
            with mock.patch.object(remote_action, 'PROJECT_ROOT', fake_root):
                action = remote_action.RemoteAction(
                    rewrapper='/path/to/rewrapper',
                    command=command,
                )
                self.assertEqual(action.exec_root, fake_root)
                self.assertEqual(action.build_subdir, fake_builddir)

    def test_path_setup_explicit(self):
        command = ['beep', 'boop']
        fake_root = '/home/project'
        fake_builddir = 'out/not-default'
        fake_cwd = os.path.join(fake_root, fake_builddir)
        with mock.patch.object(os, 'curdir', fake_cwd):
            action = remote_action.RemoteAction(
                rewrapper='/path/to/rewrapper',
                command=command,
                exec_root=fake_root,
            )
            self.assertEqual(action.exec_root, fake_root)
            self.assertEqual(action.build_subdir, fake_builddir)

    @mock.patch.object(os, 'curdir', _WORKING_DIR)
    def test_inputs_outputs(self):
        command = ['cat', '../src/meow.txt']
        action = remote_action.RemoteAction(
            rewrapper='/path/to/rewrapper',
            command=command,
            exec_root=self._PROJECT_ROOT,
            inputs=['../src/meow.txt'],
            output_files=['obj/woof.txt'],
            output_dirs=['.debug'],
        )
        self.assertEqual(action.build_subdir, 'build_dir')
        self.assertEqual(
            action.inputs_relative_to_project_root, ['src/meow.txt'])
        self.assertEqual(
            action.output_files_relative_to_project_root,
            ['build_dir/obj/woof.txt'])
        self.assertEqual(
            action.output_dirs_relative_to_project_root, ['build_dir/.debug'])
        with mock.patch.object(
                remote_action.RemoteAction, '_inputs_list_file',
                return_value='obj/woof.txt.inputs') as mock_input_list_file:
            self.assertEqual(
                action.command, [
                    '/path/to/rewrapper',
                    f'--exec_root={self._PROJECT_ROOT}',
                    '--input_list_paths=obj/woof.txt.inputs',
                    '--output_files=build_dir/obj/woof.txt',
                    '--output_directories=build_dir/.debug',
                    '--',
                    'cat',
                    '../src/meow.txt',
                ])
            mock_input_list_file.assert_called_once()
            with mock.patch.object(subprocess, 'call',
                                   return_value=0) as mock_call:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(action.run(), 0)
                    mock_call.assert_called_once()
                    mock_cleanup.assert_called_once()

    def test_save_temps(self):
        command = ['echo', 'hello']
        action = remote_action.RemoteAction(
            rewrapper='/path/to/rewrapper',
            command=command,
            exec_root=self._PROJECT_ROOT,
            save_temps=True,
        )
        self.assertEqual(action.local_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)
        self.assertTrue(action.save_temps)
        with mock.patch.object(subprocess, 'call', return_value=0) as mock_call:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                self.assertEqual(action.run(), 0)
                mock_call.assert_called_once()
                mock_cleanup.assert_not_called()

    @mock.patch.object(os, 'curdir', _WORKING_DIR)
    def test_flag_forwarding(self):
        command = [
            'cat', '--remote-flag=--exec_strategy=racing', '../src/cow/moo.txt'
        ]
        action = remote_action.RemoteAction(
            rewrapper='/path/to/rewrapper',
            command=command,
            exec_root=self._PROJECT_ROOT,
        )
        self.assertEqual(action.local_command, ['cat', '../src/cow/moo.txt'])
        self.assertEqual(action.options, ['--exec_strategy=racing'])


if __name__ == '__main__':
    unittest.main()
