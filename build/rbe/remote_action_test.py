#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import fuchsia
import remote_action
import cl_utils
import output_leak_scanner
import remotetool

from typing import Any, Dict, Sequence

_HAVE_XATTR = hasattr(os, 'setxattr')


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


class FakeReproxyLogEntry(remote_action.ReproxyLogEntry):
    """Mimic a ReproxyLogEntry by setting properties without parsing."""

    def __init__(self, **kwargs):
        # intentionally does not call super().__init__(), but instead
        # sets property attributes.
        for k, v in kwargs.items():
            setattr(self, '_' + k, v)

    @property
    def execution_id(self) -> str:
        return self._execution_id

    @property
    def action_digest(self) -> str:
        return self._action_digest

    @property
    def output_file_digests(self) -> Dict[Path, str]:
        return self._output_file_digests

    @property
    def output_directory_digests(self) -> Dict[Path, str]:
        return self._output_directory_digests

    @property
    def completion_status(self) -> str:
        return self._completion_status


class FileMatchTests(unittest.TestCase):

    def test_match(self):
        with tempfile.TemporaryDirectory() as td:
            f1path = Path(td, 'left.txt')
            f2path = Path(td, 'right.txt')
            _write_file_contents(f1path, 'a\n')
            _write_file_contents(f2path, 'a\n')
            self.assertTrue(remote_action._files_match(f1path, f2path))
            self.assertTrue(remote_action._files_match(f2path, f1path))

    def test_not_match(self):
        with tempfile.TemporaryDirectory() as td:
            f1path = Path(td, 'left.txt')
            f2path = Path(td, 'right.txt')
            _write_file_contents(f1path, 'a\n')
            _write_file_contents(f2path, 'b\n')
            self.assertFalse(remote_action._files_match(f1path, f2path))
            self.assertFalse(remote_action._files_match(f2path, f1path))


class DetailDiffTests(unittest.TestCase):

    def test_called(self):
        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(0)) as mock_call:
            self.assertEqual(
                remote_action._detail_diff(
                    Path('file1.txt'), Path('file2.txt')).returncode, 0)
        mock_call.assert_called_once()
        first_call = mock_call.call_args_list[0]
        args, unused_kwargs = first_call
        command = args[0]  # list
        self.assertTrue(
            command[0].endswith(str(remote_action._DETAIL_DIFF_SCRIPT)))

    def test_filtered(self):

        def _filter_for_compare(
                file1: Path, filtered1: Path, file2: Path,
                filtered2: Path) -> bool:
            # Pretend we wrote filtered views to filtered1 and filtered2.
            return True

        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(0)) as mock_call:
            self.assertEqual(
                remote_action._detail_diff_filtered(
                    Path('file1.txt'),
                    Path('file2.txt'),
                    maybe_transform_pair=_filter_for_compare).returncode, 0)
        mock_call.assert_called_once()
        first_call = mock_call.call_args_list[0]
        args, unused_kwargs = first_call
        command = args[0]  # list
        self.assertTrue(
            command[0].endswith(str(remote_action._DETAIL_DIFF_SCRIPT)))
        self.assertEqual(command[1], 'file1.txt.filtered')
        self.assertEqual(command[2], 'file2.txt.filtered')


class TextDiffTests(unittest.TestCase):

    def test_called(self):
        result = cl_utils.SubprocessResult(0)
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=result) as mock_call:
            self.assertEqual(
                remote_action._text_diff('file1.txt', 'file2.txt'), result)
        mock_call.assert_called_once()
        first_call = mock_call.call_args_list[0]
        args, unused_kwargs = first_call
        command = args[0]  # list
        self.assertEqual(command[0], 'diff')
        self.assertEqual(command[-2:], ['file1.txt', 'file2.txt'])

    def test_matches(self):  # no mocking
        with tempfile.TemporaryDirectory() as td:
            f1 = Path(td) / 'left.txt'
            f2 = Path(td) / 'right.txt'
            contents = 'The quick brown fox\njumped over the lazy\ndogs.\n'
            _write_file_contents(f1, contents)
            _write_file_contents(f2, contents)
            result = remote_action._text_diff(f1, f2)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout, [])

    def test_not_matches(self):  # no mocking
        with tempfile.TemporaryDirectory() as td:
            f1 = Path(td) / 'left.txt'
            f2 = Path(td) / 'right.txt'
            contents = 'The quick brown fox\njumped over the lazy\ndogs.\n'
            _write_file_contents(f1, contents)
            _write_file_contents(f2, contents.replace('m', 'M'))
            result = remote_action._text_diff(f1, f2)
            self.assertEqual(result.returncode, 1)
            self.assertNotEqual(result.stdout, [])


class FilesUnderDirTests(unittest.TestCase):

    def test_walk(self):
        with tempfile.TemporaryDirectory() as td:
            f1path = Path(td) / 'left.txt'
            subdir = Path(td) / 'sub'
            os.mkdir(subdir)
            f2path = subdir / 'right.txt'
            _write_file_contents(f1path, '\n')
            _write_file_contents(f2path, '\n')
            self.assertEqual(
                set(remote_action._files_under_dir(td)),
                _paths({'left.txt', 'sub/right.txt'}))


class CommonFilesUnderDirsTests(unittest.TestCase):

    def test_none_in_common(self):
        with mock.patch.object(remote_action, '_files_under_dir',
                               side_effect=[iter(['a', 'b', 'c']),
                                            iter(['d', 'e', 'f'])]) as mock_lsr:
            self.assertEqual(
                remote_action._common_files_under_dirs('foo-dir', 'bar-dir'),
                set())

    def test_some_in_common(self):
        with mock.patch.object(remote_action, '_files_under_dir',
                               side_effect=[iter(_paths(['a', 'b/x', 'c'])),
                                            iter(_paths(['d', 'c',
                                                         'b/x']))]) as mock_lsr:
            self.assertEqual(
                remote_action._common_files_under_dirs(
                    Path('foo-dir'), Path('bar-dir')), _paths({'b/x', 'c'}))


class ExpandCommonFilesBetweenDirs(unittest.TestCase):

    def test_common(self):
        # Normally returns a set, but mock-return a list for deterministic
        # ordering.
        with mock.patch.object(remote_action, '_common_files_under_dirs',
                               return_value=_paths(['y/z', 'x'])) as mock_ls:
            self.assertEqual(
                list(
                    remote_action._expand_common_files_between_dirs(
                        [_paths(('c', 'd')),
                         _paths(('a', 'b'))])), [
                             _paths(('c/x', 'd/x')),
                             _paths(('c/y/z', 'd/y/z')),
                             _paths(('a/x', 'b/x')),
                             _paths(('a/y/z', 'b/y/z')),
                         ])


class FileLinesMatchingTests(unittest.TestCase):

    def test_empty(self):
        with mock.patch('builtins.open',
                        mock.mock_open(read_data='')) as mock_file:
            self.assertEqual(
                list(
                    remote_action._file_lines_matching(
                        'log.txt', 'never-match')),
                [],
            )

    def test_matches(self):
        with mock.patch('builtins.open',
                        mock.mock_open(read_data='ab\nbc\ncd\n')) as mock_file:
            self.assertEqual(
                list(remote_action._file_lines_matching('file.txt', 'c')),
                ['bc\n', 'cd\n'],
            )


class TransformFileByLines(unittest.TestCase):

    def test_no_change(self):
        with tempfile.TemporaryDirectory() as td:
            f1 = Path(td) / 'in.txt'
            f2 = Path(td) / 'out.txt'
            _write_file_contents(
                f1, 'aa\n\n\nbb\ncc dd\n\ne f \n gh ij\n  k  l  \n')
            remote_action._transform_file_by_lines(f1, f2, lambda x: x)
            s1 = _read_file_contents(f1)
            s2 = _read_file_contents(f2)
            self.assertEqual(s1, s2)


class ReclientCanonicalWorkingDirTests(unittest.TestCase):

    def test_empty(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir(Path('')), Path(''))

    def test_one_level(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir(Path('build-here')),
            Path('set_by_reclient'))

    def test_two_levels(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir(Path('build/there')),
            Path('set_by_reclient/a'))

    def test_three_levels(self):
        self.assertEqual(
            remote_action.reclient_canonical_working_dir(
                Path('build/inside/there')), Path('set_by_reclient/a/a'))


class RewriteDepfileTests(unittest.TestCase):

    def test_depfile_in_place(self):
        with tempfile.TemporaryDirectory() as td:
            depfile = Path(td) / 'dep.d'

            wd = Path('/home/base/out/inside/here')
            _write_file_contents(
                depfile, f'obj/foo.o: {wd}/foo/bar.h {wd}/baz/quux.h\n')
            remote_action.rewrite_depfile(
                depfile,
                transform=lambda x: remote_action._remove_prefix(x, f'{wd}/')
            )  # write in-place
            self.assertEqual(
                _read_file_contents(depfile),
                'obj/foo.o: foo/bar.h baz/quux.h\n',
            )

    def test_depfile_new_file(self):
        with tempfile.TemporaryDirectory() as td:
            depfile = Path(td) / 'dep.d'
            output = depfile.with_suffix('.new')

            wd = Path('/all/your/base')
            _write_file_contents(
                depfile, f'obj/foo.o: {wd}/foo/bar.h {wd}/baz/quux.h\n')
            remote_action.rewrite_depfile(
                depfile,
                transform=lambda x: remote_action._remove_prefix(x, f'{wd}/'),
                output=output)  # write new file
            self.assertEqual(
                _read_file_contents(output),
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
            _paths(
                [
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
                    '/lib/x86_64-linux-gnu/libdl.so.2',
                    '/lib/x86_64-linux-gnu/librt.so.1',
                    '/lib/x86_64-linux-gnu/libpthread.so.0',
                    '/lib/x86_64-linux-gnu/libc.so.6',
                    '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
                    '/lib/x86_64-linux-gnu/libm.so.6',
                ]))


class HostToolNonsystemShlibsTests(unittest.TestCase):

    def test_sample(self):
        unfiltered_shlibs = _paths(
            [
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
                '/lib/x86_64-linux-gnu/libdl.so.2',
                '/lib/x86_64-linux-gnu/librt.so.1',
                '/lib/x86_64-linux-gnu/libpthread.so.0',
                '/lib/x86_64-linux-gnu/libc.so.6',
                '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
                '/lib/x86_64-linux-gnu/libm.so.6',
                '/usr/lib/something_else.so',
            ])
        with mock.patch.object(
                remote_action, 'host_tool_shlibs',
                return_value=unfiltered_shlibs) as mock_host_tool_shlibs:
            self.assertEqual(
                list(
                    remote_action.host_tool_nonsystem_shlibs(
                        '../path/to/rustc')),
                _paths(
                    [
                        '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/librustc_driver-897e90da9cc472c4.so',
                        '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/libstd-374958b5d3497a8f.so',
                        '/usr/home/janedoe/my_project/tools/rust/linux-x64/bin/../lib/../lib/libLLVM-15-rust-1.70.0-nightly.so',
                    ]))
        mock_host_tool_shlibs.assert_called_once()


class RewrapperArgParserTests(unittest.TestCase):

    @property
    def _parser(self):
        return remote_action._REWRAPPER_ARG_PARSER

    def test_default(self):
        args, _ = self._parser.parse_known_args([])
        self.assertIsNone(args.exec_root)
        self.assertIsNone(args.canonicalize_working_dir)

    def test_exec_root(self):
        args, _ = self._parser.parse_known_args(['--exec_root=/foo/bar'])
        self.assertEqual(args.exec_root, '/foo/bar')

    def test_canonicalize_working_dir_true(self):
        args, _ = self._parser.parse_known_args(
            ['--canonicalize_working_dir=true'])
        self.assertTrue(args.canonicalize_working_dir)

    def test_canonicalize_working_dir_false(self):
        args, _ = self._parser.parse_known_args(
            ['--canonicalize_working_dir=false'])
        self.assertFalse(args.canonicalize_working_dir)

    def test_help_unwanted(self):
        for opt in ('-h', '--help'):
            with mock.patch.object(sys, 'exit') as mock_exit:
                self._parser.parse_known_args([opt])
            mock_exit.assert_not_called()


class RemoteActionMainParserTests(unittest.TestCase):

    @property
    def default_cfg(self):
        return Path('default.cfg')

    @property
    def default_bindir(self):
        return Path('/opt/reclient/bin')

    def _make_main_parser(self) -> argparse.ArgumentParser:
        parser = argparse.ArgumentParser()
        remote_action.inherit_main_arg_parser_flags(
            parser,
            default_cfg=self.default_cfg,
            default_bindir=self.default_bindir)
        return parser

    def test_defaults(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--', 'echo', 'hello'])
        self.assertEqual(main_args.cfg, self.default_cfg)
        self.assertEqual(main_args.bindir, self.default_bindir)
        self.assertFalse(main_args.dry_run)
        self.assertFalse(main_args.verbose)
        self.assertEqual(main_args.label, '')
        self.assertEqual(main_args.remote_log, '')
        self.assertFalse(main_args.save_temps)
        self.assertIsNone(main_args.fsatrace_path)
        self.assertFalse(main_args.compare)
        self.assertFalse(main_args.diagnose_nonzero)
        self.assertEqual(main_args.command, ['echo', 'hello'])
        self.assertIsNone(main_args.remote_debug_command)

    def test_cfg(self):
        p = self._make_main_parser()
        cfg = Path('other.cfg')
        main_args, other = p.parse_known_args([f'--cfg={cfg}', '--', 'echo'])
        self.assertEqual(main_args.cfg, cfg)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_only_command, ['echo'])
        self.assertEqual(action.options, ['--cfg', str(cfg)])

    def test_bindir(self):
        p = self._make_main_parser()
        bindir = Path('/usr/local/bin')
        main_args, other = p.parse_known_args(
            ['--bindir', str(bindir), '--', 'echo'])
        self.assertEqual(main_args.bindir, bindir)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_only_command, ['echo'])

    def test_local_command_with_env(self):
        local_command = (['FOO=BAR', 'echo'])
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--'] + local_command)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(
            action.local_only_command, [cl_utils._ENV] + local_command)
        self.assertEqual(
            action.remote_only_command, [cl_utils._ENV] + local_command)

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
        # --auto-reproxy is now obsolete, and will be removed in the future
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--auto-reproxy', '--', 'echo'])
        self.assertTrue(main_args.auto_reproxy)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_only_command, ['echo'])
        rewrapper_prefix, _, remote_command = cl_utils.partition_sequence(
            action.launch_command, '--')
        self.assertEqual(Path(rewrapper_prefix[0]).name, 'rewrapper')
        self.assertEqual(remote_command, ['echo'])

    def test_save_temps(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--save-temps', '--', 'echo'])
        self.assertTrue(main_args.save_temps)
        action = remote_action.remote_action_from_args(main_args)
        self.assertEqual(action.local_only_command, ['echo'])
        self.assertTrue(action.save_temps)

    def test_label(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--label=//build/this:that', '--', 'echo'])
        self.assertEqual(main_args.label, '//build/this:that')

    def test_diagnose_nonzero(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--diagnose-nonzero', '--', 'echo'])
        self.assertTrue(main_args.diagnose_nonzero)
        action = remote_action.remote_action_from_args(main_args)
        self.assertTrue(action.diagnose_nonzero)

    def test_input_list_paths(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        input1 = Path('hello.txt')
        input2 = Path('goodbye.txt')
        with tempfile.TemporaryDirectory() as td:
            rspfile1 = Path(td) / 'inputs.rsp'
            _write_file_contents(rspfile1, f'{input1}\n')
            rspfile2 = Path(td) / 'more-inputs.rsp'
            _write_file_contents(rspfile2, f'{input2}\n')

            p = self._make_main_parser()
            main_args, other = p.parse_known_args(
                _strs(
                    [
                        f'--input_list_paths={rspfile1},{rspfile2}', '--',
                        'cat', input1, input2
                    ]))
            action = remote_action.remote_action_from_args(
                main_args,
                exec_root=exec_root,
                working_dir=working_dir,
            )

            self.assertEqual(
                set(action.inputs_relative_to_project_root),
                {build_dir / input1, build_dir / input2
                },  # relative to exec_root
            )

    def test_remote_debug_command(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        input = Path('hello.txt')
        debug = "ls -l -R .."
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            _strs([f'--remote-debug-command={debug}', '--', 'cat', input]))
        action = remote_action.remote_action_from_args(
            main_args,
            inputs=[input],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(action.remote_debug_command, debug.split())
        self.assertEqual(
            {build_dir / input}, set(action.inputs_relative_to_project_root))
        with mock.patch.object(cl_utils, 'subprocess_call') as mock_remote:
            self.assertEqual(action.run(), 1)
        mock_remote.assert_called_once()
        arg, kwargs = mock_remote.call_args_list[0]
        full_command = arg[0]
        rewrapper_prefix, sep, remote_command = cl_utils.partition_sequence(
            full_command, '--')
        self.assertEqual(remote_command, debug.split())

    def test_remote_log_named(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--log', 'bar.remote-log', '--', 'echo'])
        self.assertEqual(main_args.remote_log, 'bar.remote-log')

    def test_remote_log_unnamed(self):
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--log', '--', 'echo'])
        self.assertEqual(main_args.remote_log, '<AUTO>')

    def test_remote_log_from_main_args_auto_named(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        command = ['touch', str(output)]
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(['--log', '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(
            [remote_action._REMOTE_LOG_SCRIPT],
            action.inputs_relative_to_project_root)
        self.assertEqual(
            {build_dir / output, build_dir / (str(output) + '.remote-log')},
            set(action.output_files_relative_to_project_root))
        # Ignore the rewrapper portion of the command
        full_command = action.launch_command
        command_slices = list(
            cl_utils.split_into_subsequences(full_command, '--'))
        prefix, log_wrapper, main_command = command_slices
        # Confirm that the remote command is wrapped with the logger script.
        self.assertEqual(
            log_wrapper,
            _strs(
                [
                    Path('..', remote_action._REMOTE_LOG_SCRIPT), '--log',
                    str(output) + '.remote-log'
                ]))
        self.assertEqual(main_command, command)

    def test_remote_log_from_main_args_explicitly_named(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        log_base = 'debug'
        p = self._make_main_parser()
        command = ['touch', str(output)]
        main_args, other = p.parse_known_args(
            ['--log', log_base, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(
            [remote_action._REMOTE_LOG_SCRIPT],
            action.inputs_relative_to_project_root)
        self.assertEqual(
            {
                build_dir / output,
                build_dir / (log_base + '.remote-log'),
            }, set(action.output_files_relative_to_project_root))
        # Ignore the rewrapper portion of the command
        command_slices = list(
            cl_utils.split_into_subsequences(action.launch_command, '--'))
        prefix, log_wrapper, main_command = command_slices
        # Confirm that the remote command is wrapped with the logger script.
        self.assertEqual(
            log_wrapper,
            _strs(
                [
                    Path('..', remote_action._REMOTE_LOG_SCRIPT), '--log',
                    log_base + '.remote-log'
                ]))
        self.assertEqual(main_command, command)

    def test_remote_fsatrace_path_default(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        fake_fsatrace = fuchsia.FSATRACE_PATH
        fake_fsatrace_rel = Path(f'../{fake_fsatrace}')
        p = self._make_main_parser()
        command = ['touch', str(output)]
        # Pass "" to use the default fuchsia.FSATRACE_PATH
        main_args, other = p.parse_known_args(
            ['--fsatrace-path=', '--'] + command)
        self.assertEqual(main_args.fsatrace_path, Path(''))
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(
            {fake_fsatrace, fake_fsatrace.with_suffix('.so')},
            set(action.inputs_relative_to_project_root))
        self.assertEqual(
            {
                build_dir / output,  # relative to exec_root
                build_dir / (str(output) + '.remote-fsatrace')
            },
            set(action.output_files_relative_to_project_root))
        # Ignore the rewrapper portion of the command
        cmd_slices = cl_utils.split_into_subsequences(
            action.launch_command, '--')
        rewrapper_prefix, fsatrace_prefix, remote_command = cmd_slices
        # Confirm that the remote command is wrapped with fsatrace
        self.assertIn(str(fake_fsatrace_rel), fsatrace_prefix)
        self.assertEqual(
            fsatrace_prefix + ['--'],
            action._fsatrace_command_prefix(
                Path(str(output) + '.remote-fsatrace')))
        self.assertEqual(remote_command, command)

    def test_remote_fsatrace_from_main_args(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        fake_fsatrace = Path('tools/debug/fsatrace')
        fake_fsatrace_rel = Path(f'../{fake_fsatrace}')
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            _strs(
                ['--fsatrace-path', fake_fsatrace_rel, '--', 'touch', output]))
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(
            {fake_fsatrace, fake_fsatrace.with_suffix('.so')},
            set(action.inputs_relative_to_project_root))
        self.assertEqual(
            {
                build_dir / output, build_dir /
                (str(output) + '.remote-fsatrace')
            }, set(action.output_files_relative_to_project_root))
        # Ignore the rewrapper portion of the command
        full_command = action.launch_command
        command_slices = list(
            cl_utils.split_into_subsequences(action.launch_command, '--'))
        prefix, trace_wrapper, main_command = command_slices
        # Confirm that the remote command is wrapped with fsatrace
        self.assertIn(str(fake_fsatrace_rel), trace_wrapper)
        self.assertEqual(
            trace_wrapper + ['--'],
            action._fsatrace_command_prefix(
                Path(str(output) + '.remote-fsatrace')))
        self.assertEqual(main_command, ['touch', str(output)])

    def test_remote_log_and_fsatrace_from_main_args(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        command = ['touch', str(output)]
        fake_fsatrace = Path('tools/debug/fsatrace')
        fake_fsatrace_rel = Path('..', fake_fsatrace)
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            _strs(
                ['--fsatrace-path', fake_fsatrace_rel, '--log', '--'] +
                command))
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        self.assertEqual(
            {
                remote_action._REMOTE_LOG_SCRIPT,
                fake_fsatrace,
                fake_fsatrace.with_suffix('.so'),
            }, set(action.inputs_relative_to_project_root))
        self.assertEqual(
            {
                build_dir / output,
                build_dir / (str(output) + '.remote-log'),
                build_dir / (str(output) + '.remote-fsatrace'),
            }, set(action.output_files_relative_to_project_root))
        # Ignore the rewrapper portion of the command
        command_slices = list(
            cl_utils.split_into_subsequences(action.launch_command, '--'))
        rewrapper_prefix, log_wrapper, trace_wrapper, main_command = command_slices
        # Confirm that the outer wrapper is for logging
        self.assertEqual(
            log_wrapper,
            _strs(
                [
                    Path('..', remote_action._REMOTE_LOG_SCRIPT), '--log',
                    str(output) + '.remote-log'
                ]))
        # Confirm that the inner wrapper is for fsatrace
        self.assertEqual(
            trace_wrapper + ['--'],
            action._fsatrace_command_prefix(
                Path(str(output) + '.remote-fsatrace')))
        self.assertEqual(main_command, command)

    def test_local_only_no_compare(self):
        # --compare does nothing with --local
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        base_command = ['touch', str(output)]
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--compare', '--local', '--'] + base_command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.remote_disable)
        self.assertTrue(action.compare_with_local)

        with mock.patch.object(remote_action.RemoteAction, 'run',
                               return_value=0) as mock_run:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_compare_against_local') as mock_compare:
                exit_code = action.run_with_main_args(main_args)

        self.assertEqual(exit_code, 0)
        mock_run.assert_called_once()
        mock_compare.assert_not_called()

    def test_compare_fsatraces_acceptable_match(self):
        exec_root = Path('/home/project')
        build_dir = Path('build/out/here')
        working_dir = exec_root / build_dir
        p = self._make_main_parser()
        action = remote_action.RemoteAction(
            rewrapper=Path('/test-build/rewrapper'),
            command=['sleep', '1h'],
            options=['--canonicalize_working_dir=true'],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.canonicalize_working_dir)
        local_trace_contents = f"""r|{exec_root}/src/input.c
w|{working_dir}/obj/input.o
"""
        remote_root = remote_action._REMOTE_PROJECT_ROOT
        remote_trace_contents = f"""r|{remote_root}/src/input.c
w|{remote_root}/set_by_reclient/a/a/obj/input.o
"""
        self.assertNotEqual(local_trace_contents, remote_trace_contents)
        with tempfile.TemporaryDirectory() as td:
            local_trace = Path(td) / 'local.trace'
            remote_trace = Path(td) / 'remote.trace'
            _write_file_contents(local_trace, local_trace_contents)
            _write_file_contents(remote_trace, remote_trace_contents)
            diff_text = io.StringIO()
            with contextlib.redirect_stdout(diff_text):
                status = action._compare_fsatraces_select_logs(
                    local_trace=local_trace,
                    remote_trace=remote_trace,
                )
        self.assertEqual(status.returncode, 0)  # contents are equivalent

    def test_compare_fsatraces_with_difference(self):
        exec_root = Path('/home/project')
        build_dir = Path('build/out/here')
        working_dir = exec_root / build_dir
        p = self._make_main_parser()
        action = remote_action.RemoteAction(
            rewrapper=Path('/test-build/rewrapper'),
            command=['sleep', '1h'],
            options=['--canonicalize_working_dir=true'],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.canonicalize_working_dir)
        local_trace_contents = f"""r|{exec_root}/src/input.c
w|{working_dir}/obj/input.o
"""
        remote_root = remote_action._REMOTE_PROJECT_ROOT
        remote_trace_contents = f"""r|{remote_root}/src/input.c
r|{remote_root}/includes/input.h
w|{remote_root}/set_by_reclient/a/a/obj/input.o
"""
        self.assertNotEqual(local_trace_contents, remote_trace_contents)
        with tempfile.TemporaryDirectory() as td:
            local_trace = Path(td) / 'local.trace'
            remote_trace = Path(td) / 'remote.trace'
            _write_file_contents(local_trace, local_trace_contents)
            _write_file_contents(remote_trace, remote_trace_contents)
            diff_text = io.StringIO()
            with contextlib.redirect_stdout(diff_text):
                result = action._compare_fsatraces_select_logs(
                    local_trace=local_trace,
                    remote_trace=remote_trace,
                )
        self.assertEqual(result.returncode, 1)  # traces differ

    def test_local_remote_compare_no_diffs_from_main_args(self):
        # Same as test_remote_fsatrace_from_main_args, but with --compare
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        base_command = ['touch', str(output)]
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--compare', '--'] + base_command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.compare_with_local)

        unnamed_mocks = [
            # we don't bother to check the call details of these mocks
            mock.patch.object(Path, 'rename'),
            mock.patch.object(Path, 'is_file', return_value=True),
            # Pretend comparison finds no differences
            mock.patch.object(remote_action, '_files_match', return_value=True),
        ]
        with contextlib.ExitStack() as stack:
            for m in unnamed_mocks:
                stack.enter_context(m)

            # both local and remote commands succeed
            with mock.patch.object(remote_action.RemoteAction, '_run_locally',
                                   return_value=0) as mock_local_launch:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_run_maybe_remotely',
                                       return_value=cl_utils.SubprocessResult(
                                           0)) as mock_remote_launch:
                    with mock.patch.object(
                            remote_action.RemoteAction,
                            '_compare_fsatraces') as mock_compare_traces:
                        with mock.patch.object(os, 'remove') as mock_cleanup:
                            exit_code = action.run_with_main_args(main_args)

        remote_command = action.launch_command
        self.assertEqual(exit_code, 0)  # remote success and compare success
        mock_compare_traces.assert_not_called()
        mock_local_launch.assert_called_once()
        mock_remote_launch.assert_called_once()
        self.assertEqual(remote_command[-2:], base_command)
        mock_cleanup.assert_called_with(Path(str(output) + '.remote'))

    def test_local_remote_compare_found_diffs_from_main_args(self):
        # Same as test_remote_fsatrace_from_main_args, but with --compare
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        base_command = ['touch', str(output)]
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--compare', '--'] + base_command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.compare_with_local)

        unnamed_mocks = [
            # we don't bother to check the call details of these mocks
            mock.patch.object(Path, 'rename'),
            mock.patch.object(Path, 'is_file', return_value=True),
            # Pretend comparison finds differences
            mock.patch.object(
                remote_action, '_files_match', return_value=False),
            mock.patch.object(remote_action, '_detail_diff'),
        ]
        with contextlib.ExitStack() as stack:
            for m in unnamed_mocks:
                stack.enter_context(m)

            # both local and remote commands succeed
            with mock.patch.object(remote_action.RemoteAction, '_run_locally',
                                   return_value=0) as mock_local_launch:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_run_maybe_remotely',
                                       return_value=cl_utils.SubprocessResult(
                                           0)) as mock_remote_launch:
                    with mock.patch.object(
                            remote_action.RemoteAction,
                            '_compare_fsatraces') as mock_compare_traces:
                        exit_code = action.run_with_main_args(main_args)

        remote_command = action.launch_command
        self.assertEqual(exit_code, 1)  # remote success, but compare failure
        mock_compare_traces.assert_not_called()
        mock_local_launch.assert_called_once()
        mock_remote_launch.assert_called_once()
        self.assertEqual(remote_command[-2:], base_command)

    def test_local_remote_compare_with_fsatrace_from_main_args(self):
        # Same as test_remote_fsatrace_from_main_args, but with --compare
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        output = Path('hello.txt')
        fake_fsatrace = Path('tools/debug/fsatrace')
        fake_fsatrace_rel = Path('..', fake_fsatrace)
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            _strs(
                [
                    '--compare', '--fsatrace-path', fake_fsatrace_rel, '--',
                    'touch', output
                ]))
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )

        # not repeating the same asserts from
        #   test_remote_fsatrace_from_main_args:

        unnamed_mocks = [
            # we don't bother to check the call details of these mocks
            mock.patch.object(Path, 'rename'),
            mock.patch.object(Path, 'is_file', return_value=True),
            # Pretend comparison finds differences
            mock.patch.object(
                remote_action, '_files_match', return_value=False),
            mock.patch.object(remote_action, '_detail_diff'),
            # in RemoteAction._compare_fsatraces:
            mock.patch.object(remote_action, '_transform_file_by_lines'),
        ]
        with contextlib.ExitStack() as stack:
            for m in unnamed_mocks:
                stack.enter_context(m)

            # both local and remote commands succeed
            with mock.patch.object(remote_action.RemoteAction, '_run_locally',
                                   return_value=0) as mock_local_launch:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_run_maybe_remotely',
                                       return_value=cl_utils.SubprocessResult(
                                           0)) as mock_remote_launch:
                    with mock.patch.object(
                            remote_action, '_text_diff',
                            return_value=cl_utils.SubprocessResult(
                                0)) as mock_trace_diff:
                        exit_code = action.run_with_main_args(main_args)

        remote_command = action.launch_command
        # make sure local command is also traced
        local_command = list(action._generate_local_launch_command())
        self.assertEqual(exit_code, 1)  # remote success, but compare failure
        mock_remote_launch.assert_called_once()
        mock_local_launch.assert_called_once()
        self.assertIn(str(fake_fsatrace_rel), remote_command)
        self.assertIn(str(fake_fsatrace_rel), local_command)
        remote_trace = str(output) + '.remote-fsatrace'
        local_trace = str(output) + '.local-fsatrace'
        self.assertIn(remote_trace, remote_command)
        self.assertIn(local_trace, local_command)
        mock_trace_diff.assert_called_with(
            Path(local_trace + '.norm'), Path(remote_trace + '.norm'))

    def test_local_check_determinism(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        exec_root_rel = cl_utils.relpath(exec_root, start=working_dir)
        output = Path('hello.txt')
        base_command = ['touch', str(output)]
        p = self._make_main_parser()
        main_args, other = p.parse_known_args(
            ['--check-determinism', '--local', '--'] + base_command)
        action = remote_action.remote_action_from_args(
            main_args,
            output_files=[output],
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertTrue(action.remote_disable)

        with mock.patch.object(
                cl_utils, 'subprocess_call',
                return_value=cl_utils.SubprocessResult(0)) as mock_run:
            exit_code = action.run_with_main_args(main_args)

        self.assertEqual(exit_code, 0)
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        launch_command = args[0]
        self.assertEqual(kwargs['cwd'], working_dir)
        check_prefix, sep, main_command = cl_utils.partition_sequence(
            launch_command, '--')
        self.assertEqual(check_prefix[0], sys.executable)
        self.assertIn(
            str(exec_root_rel / remote_action._CHECK_DETERMINISM_SCRIPT),
            check_prefix)
        self.assertIn('--check-repeatability', check_prefix)
        _, _, output_list = cl_utils.partition_sequence(
            check_prefix, '--outputs')
        self.assertEqual(output_list, [str(output)])
        self.assertEqual(main_command, base_command)

    def test_output_leak_scan_with_canonical_working_dir_mocked(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        canonical_dir_option = '--canonicalize_working_dir=true'
        p = self._make_main_parser()
        command = ['echo']
        main_args, other = p.parse_known_args(
            [canonical_dir_option, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertEqual(action.local_only_command, command)
        self.assertTrue(action.canonicalize_working_dir)
        self.assertIn(canonical_dir_option, action.options)
        with mock.patch.object(output_leak_scanner, 'preflight_checks',
                               return_value=0) as mock_scan:
            with mock.patch.object(
                    remote_action.RemoteAction, '_run_maybe_remotely',
                    return_value=cl_utils.SubprocessResult(0)) as mock_run:
                exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_scan.assert_called_with(
            paths=[],
            command=command,
            pattern=output_leak_scanner.PathPattern(action.build_subdir),
        )
        mock_run.assert_called()

    def test_output_leak_scan_skipped_when_build_subdir_is_dot(self):
        exec_root = Path('/home/project')
        working_dir = exec_root  # build_subdir == '.'
        canonical_dir_option = '--canonicalize_working_dir=true'
        p = self._make_main_parser()
        command = ['echo']
        main_args, other = p.parse_known_args(
            [canonical_dir_option, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertEqual(action.local_only_command, command)
        self.assertTrue(action.canonicalize_working_dir)
        self.assertIn(canonical_dir_option, action.options)
        with mock.patch.object(output_leak_scanner, 'preflight_checks',
                               return_value=0) as mock_scan:
            with mock.patch.object(
                    remote_action.RemoteAction, '_run_maybe_remotely',
                    return_value=cl_utils.SubprocessResult(0)) as mock_run:
                exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_scan.assert_not_called()
        mock_run.assert_called()

    def test_output_leak_scan_with_canonical_working_dir_called(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        canonical_dir_option = '--canonicalize_working_dir=true'
        p = self._make_main_parser()
        command = ['echo']
        main_args, other = p.parse_known_args(
            [canonical_dir_option, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertEqual(action.local_only_command, command)
        self.assertTrue(action.canonicalize_working_dir)
        self.assertIn(canonical_dir_option, action.options)
        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(0)) as mock_run:
            exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_run.assert_called()

    def test_output_leak_scan_with_error_stops_run(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        canonical_dir_option = '--canonicalize_working_dir=true'
        p = self._make_main_parser()
        command = ['echo', str(build_dir)]  # command leaks build_dir
        main_args, other = p.parse_known_args(
            [canonical_dir_option, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
        )
        self.assertEqual(action.local_only_command, command)
        self.assertTrue(action.canonicalize_working_dir)
        self.assertIn(canonical_dir_option, action.options)
        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(0)) as mock_run:
            exit_code = action.run()
        self.assertEqual(exit_code, 1)  # due to output_leak_scanner
        # The output_leak_scan error stopped execution.
        mock_run.assert_not_called()


class RemoteActionFlagParserTests(unittest.TestCase):

    def _forward_and_parse(self, command):
        forwarded, filtered = remote_action.forward_remote_flags(
            ['--'] + command)
        main_args, unknown = remote_action._MAIN_ARG_PARSER.parse_known_args(
            forwarded)
        return main_args, unknown, filtered

    def test_defaults(self):
        remote_args, unknown, other = self._forward_and_parse([])
        self.assertFalse(remote_args.local)
        self.assertEqual(remote_args.inputs, [])
        self.assertEqual(remote_args.output_files, [])
        self.assertEqual(remote_args.output_directories, [])
        self.assertEqual(unknown, [])
        self.assertEqual(other, [])

    def test_command_without_forwarding(self):
        command = [
            'clang++', '--target=powerpc-apple-darwin8',
            '-fcrash-diagnostics-dir=nothing/to/see/here', '-c', 'hello.cxx',
            '-o', 'hello.o'
        ]
        remote_args, unknown, other = self._forward_and_parse(command)
        self.assertFalse(remote_args.local)
        self.assertEqual(remote_args.inputs, [])
        self.assertEqual(remote_args.output_files, [])
        self.assertEqual(remote_args.output_directories, [])
        self.assertEqual(unknown, [])
        self.assertEqual(other, command)

    def test_disable(self):
        remote_args, unknown, other = self._forward_and_parse(
            ['cat', 'foo.txt', '--remote-disable'])
        self.assertTrue(remote_args.local)
        self.assertEqual(unknown, [])
        self.assertEqual(other, ['cat', 'foo.txt'])

    def test_inputs(self):
        remote_args, unknown, other = self._forward_and_parse(
            [
                'cat', '--remote-inputs=bar.txt', 'bar.txt',
                '--remote-inputs=quux.txt', 'quux.txt'
            ])
        self.assertEqual(remote_args.inputs, ['bar.txt', 'quux.txt'])
        self.assertEqual(unknown, [])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])

    def test_inputs_comma(self):
        remote_args, unknown, other = self._forward_and_parse(
            [
                'cat', '--remote-inputs=w,x', 'bar.txt', '--remote-inputs=y,z',
                'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.inputs)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(unknown, [])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])

    def test_output_files_comma(self):
        remote_args, unknown, other = self._forward_and_parse(
            [
                './generate.sh', '--remote-outputs=w,x', 'bar.txt',
                '--remote-outputs=y,z', 'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.output_files)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(unknown, [])
        self.assertEqual(other, ['./generate.sh', 'bar.txt', 'quux.txt'])

    def test_output_dirs_comma(self):
        remote_args, unknown, other = self._forward_and_parse(
            [
                './generate_dirs.sh', '--remote-output-dirs=w,x', 'bar.txt',
                '--remote-output-dirs=y,z', 'quux.txt'
            ])
        self.assertEqual(
            list(cl_utils.flatten_comma_list(remote_args.output_directories)),
            ['w', 'x', 'y', 'z'])
        self.assertEqual(unknown, [])
        self.assertEqual(other, ['./generate_dirs.sh', 'bar.txt', 'quux.txt'])

    def test_flags(self):
        remote_args, unknown, other = self._forward_and_parse(
            [
                'cat', '--remote-flag=--foo=bar', 'bar.txt',
                '--remote-flag=--opt=quux', 'quux.txt'
            ])
        self.assertEqual(unknown, ['--foo=bar', '--opt=quux'])
        self.assertEqual(other, ['cat', 'bar.txt', 'quux.txt'])


class RemoteActionConstructionTests(unittest.TestCase):

    _PROJECT_ROOT = Path('/my/project/root')
    _WORKING_DIR = _PROJECT_ROOT / 'build_dir'

    @property
    def _rewrapper(self) -> Path:
        return Path('/path/to/rewrapper')

    def _make_remote_action(
            self,
            rewrapper=None,
            command=None,
            exec_root=None,
            working_dir=None,
            **kwargs,  # RemoteAction params
    ) -> remote_action.RemoteAction:
        """Create a RemoteAction for testing with some defaults."""
        return remote_action.RemoteAction(
            rewrapper=rewrapper or self._rewrapper,
            command=command,
            exec_root=exec_root or self._PROJECT_ROOT,
            working_dir=working_dir or self._WORKING_DIR,
            **kwargs,
        )

    def test_minimal(self):
        command = ['cat', 'meow.txt']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.remote_only_command, command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)
        self.assertEqual(action.exec_root_rel, Path('..'))
        self.assertFalse(action.save_temps)
        self.assertFalse(action.remote_disable)
        self.assertEqual(action.build_subdir, Path('build_dir'))
        self.assertEqual(
            action.launch_command,
            [str(self._rewrapper), f'--exec_root={self._PROJECT_ROOT}', '--'] +
            command)
        self.assertFalse(action.compare_with_local)
        self.assertFalse(action.check_determinism)
        self.assertFalse(action.diagnose_nonzero)
        self.assertTrue(action.download_outputs)
        self.assertEqual(action.always_download, [])

    def test_path_setup_implicit(self):
        command = ['beep', 'boop']
        fake_root = Path('/home/project')
        fake_builddir = Path('out/not-default')
        fake_cwd = fake_root / fake_builddir
        with mock.patch.object(os, 'curdir', fake_cwd):
            with mock.patch.object(remote_action, 'PROJECT_ROOT', fake_root):
                action = remote_action.RemoteAction(
                    rewrapper=self._rewrapper,
                    command=command,
                )
                self.assertEqual(action.exec_root, fake_root)
                self.assertEqual(action.exec_root_rel, Path('../..'))
                self.assertEqual(action.build_subdir, fake_builddir)

    def test_path_setup_explicit_exec_root(self):
        command = ['beep', 'boop']
        fake_root = Path('/home/project')
        fake_builddir = Path('out/not-default')
        fake_cwd = fake_root / fake_builddir
        with mock.patch.object(os, 'curdir', fake_cwd):
            action = remote_action.RemoteAction(
                rewrapper=self._rewrapper,
                command=command,
                exec_root=fake_root,
            )
            self.assertEqual(action.exec_root, fake_root)
            self.assertEqual(action.exec_root_rel, Path('../..'))
            self.assertEqual(action.build_subdir, fake_builddir)

    def test_path_setup_explicit_exec_root_and_working_dir(self):
        command = ['beep', 'boop']
        fake_root = Path('/home/project')
        fake_builddir = Path('out/not-default')
        fake_cwd = fake_root / fake_builddir
        action = remote_action.RemoteAction(
            rewrapper=self._rewrapper,
            command=command,
            exec_root=fake_root,
            working_dir=fake_cwd,
        )
        self.assertEqual(action.exec_root, fake_root)
        self.assertEqual(action.exec_root_rel, Path('../..'))
        self.assertEqual(action.build_subdir, fake_builddir)
        self.assertEqual(action.working_dir, fake_cwd)
        self.assertFalse(action.canonicalize_working_dir)
        self.assertEqual(action.remote_build_subdir, fake_builddir)
        self.assertEqual(
            action.remote_working_dir,
            remote_action._REMOTE_PROJECT_ROOT / fake_builddir)

    def test_path_setup_explicit_canonicalize_working_dir(self):
        command = ['b33p', 'b00p']
        fake_root = Path('/home/project')
        fake_builddir = Path('out/not-default')
        fake_cwd = fake_root / fake_builddir
        action = remote_action.RemoteAction(
            rewrapper=self._rewrapper,
            options=['--canonicalize_working_dir=true'],
            command=command,
            exec_root=fake_root,
            working_dir=fake_cwd,
        )
        self.assertEqual(action.exec_root, fake_root)
        self.assertEqual(action.exec_root_rel, Path('../..'))
        self.assertEqual(action.build_subdir, fake_builddir)
        self.assertEqual(action.working_dir, fake_cwd)
        self.assertTrue(action.canonicalize_working_dir)
        remote_builddir = Path('set_by_reclient/a')
        self.assertEqual(action.remote_build_subdir, remote_builddir)
        self.assertEqual(
            action.remote_working_dir,
            remote_action._REMOTE_PROJECT_ROOT / remote_builddir)

    def test_inputs_outputs(self):
        command = ['cat', '../src/meow.txt']
        action = self._make_remote_action(
            command=command,
            inputs=_paths(['../src/meow.txt']),
            output_files=_paths(['obj/woof.txt']),
            output_dirs=_paths(['.debug']),
        )
        self.assertEqual(action.build_subdir, Path('build_dir'))
        self.assertEqual(
            action.inputs_relative_to_project_root, _paths(['src/meow.txt']))
        self.assertEqual(
            action.output_files_relative_to_project_root,
            _paths(['build_dir/obj/woof.txt']))
        self.assertEqual(
            action.output_dirs_relative_to_project_root,
            _paths(['build_dir/.debug']))
        with mock.patch.object(
                remote_action.RemoteAction, '_generated_inputs_list_file',
                return_value=Path(
                    'obj/woof.txt.inputs')) as mock_input_list_file:
            self.assertEqual(
                action.launch_command, [
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
            with mock.patch.object(
                    remote_action.RemoteAction, '_run_maybe_remotely',
                    return_value=cl_utils.SubprocessResult(0)) as mock_call:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(action.run(), 0)
                    mock_call.assert_called_once()
                    mock_cleanup.assert_called_once()

    def test_save_temps(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(
            command=command,
            save_temps=True,
        )
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)
        self.assertTrue(action.save_temps)
        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(0)) as mock_call:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                self.assertEqual(action.run(), 0)
                mock_call.assert_called_once()
                mock_cleanup.assert_not_called()

    def test_flag_forwarding_pass_through(self):
        # RemoteAction construction no longer forwards --remote-* flags;
        # that responsibility has been moved to
        # remote_action.forward_remote_flags().
        command = [
            'cat', '--remote-flag=--exec_strategy=racing', '../src/cow/moo.txt'
        ]
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.options, [])

    def test_remote_fail_no_retry(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        for exit_code in (1, 2):
            with mock.patch.object(remote_action.RemoteAction,
                                   '_run_maybe_remotely',
                                   return_value=cl_utils.SubprocessResult(
                                       exit_code)) as mock_call:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(action.run(), exit_code)

            mock_cleanup.assert_called_once()
            mock_call.assert_called_once()  # no retry

    def test_local_fail_no_retry(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(
            command=command,
            disable=True,  # local-only
        )
        self.assertTrue(action.remote_disable)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.launch_command, command)  # no rewrapper
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        exit_code = 4
        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(exit_code)) as mock_call:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                self.assertEqual(action.run(), exit_code)

        mock_cleanup.assert_called_once()
        mock_call.assert_called_once()  # no retry

    def test_file_not_found_no_retry(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        exit_code = 2
        with mock.patch.object(
                remote_action.RemoteAction,
                '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(
                    returncode=exit_code,
                    stderr=['ERROR: file not found: /bin/smash',
                            'going home now']),
        ) as mock_call:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                self.assertEqual(action.run(), exit_code)

        mock_cleanup.assert_called_once()
        mock_call.assert_called_once()  # no retry

    def test_fail_to_dial_retry(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        exit_code = 5
        with mock.patch.object(
                remote_action.RemoteAction,
                '_run_maybe_remotely',
                return_value=cl_utils.
                SubprocessResult(returncode=exit_code, stderr=[
                    'F0424 15:20:57.829003 1410923 main.go:112] Fail to dial unix:///b/s/w/ir/x/w/recipe_cleanup/rbedt_5k30r/reproxy.sock: context deadline exceeded',
                    'other uninteresting log message'
                ]),
        ) as mock_call:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                self.assertEqual(action.run(), exit_code)

        mock_cleanup.assert_called_once()
        mock_call.assert_called()
        self.assertEqual(len(mock_call.call_args_list), 2)

    def test_retry_once_successful(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        for exit_code in remote_action._RETRIABLE_REWRAPPER_STATUSES:
            with mock.patch.object(
                    remote_action.RemoteAction,
                    '_run_maybe_remotely',
                    side_effect=[
                        # If at first you don't succeed,
                        cl_utils.SubprocessResult(exit_code),
                        # try, try again (and succeed).
                        cl_utils.SubprocessResult(0),
                    ]) as mock_call:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(action.run(), 0)

            mock_cleanup.assert_called_once()
            # expect called twice, second time is the retry
            self.assertEqual(len(mock_call.call_args_list), 2)

    def test_retry_once_fails_again(self):
        command = ['echo', 'hello']
        action = self._make_remote_action(command=command)
        self.assertEqual(action.local_only_command, command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        for exit_code in remote_action._RETRIABLE_REWRAPPER_STATUSES:
            with mock.patch.object(
                    remote_action.RemoteAction,
                    '_run_maybe_remotely',
                    side_effect=[
                        # If at first you don't succeed,
                        cl_utils.SubprocessResult(exit_code),
                        # try, try again (and fail again).
                        cl_utils.SubprocessResult(exit_code),
                    ]) as mock_call:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(action.run(), exit_code)  # fail

            mock_cleanup.assert_called_once()
            # expect called twice, second time is the retry
            self.assertEqual(len(mock_call.call_args_list), 2)

    def _test_local_execution_strategy(
            self, exec_strategy: str, local_status: int):
        remote_command = ['echo', 'hello']
        local_command = ['repeat-after-me', 'hello']
        action = self._make_remote_action(
            command=remote_command,
            local_only_command=local_command,
            exec_strategy=exec_strategy,
        )
        self.assertEqual(action.local_only_command, local_command)
        self.assertEqual(action.remote_only_command, remote_command)
        self.assertEqual(action.exec_root, self._PROJECT_ROOT)

        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(2)) as mock_remote:
            with mock.patch.object(remote_action.RemoteAction, '_run_locally',
                                   return_value=local_status) as mock_local:
                with mock.patch.object(remote_action.RemoteAction,
                                       '_cleanup') as mock_cleanup:
                    self.assertEqual(
                        action.run(), local_status)  # fallback success

        mock_remote.assert_called_with()
        mock_local.assert_called_with()
        mock_cleanup.assert_called_with()

    def test_strategy_local_fallback_different_command_succeeds(self):
        self._test_local_execution_strategy(
            exec_strategy="remote_local_fallback",
            local_status=0,
        )

    def test_strategy_local_fallback_different_command_fails(self):
        self._test_local_execution_strategy(
            exec_strategy="remote_local_fallback",
            local_status=1,
        )

    def test_strategy_local_only_different_command_succeeds(self):
        self._test_local_execution_strategy(
            exec_strategy="local",
            local_status=0,
        )

    def test_strategy_local_only_different_command_fails(self):
        self._test_local_execution_strategy(
            exec_strategy="local",
            local_status=3,
        )


class DownloadStubsTests(unittest.TestCase):

    @property
    def downloader(self) -> remotetool.RemoteTool:
        return remotetool.RemoteTool(
            reproxy_cfg={
                "service": "foo.buildservice:443",
                "instance": "my-project/remote/instances/default",
            })

    def test_create_file_stub_and_download(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            p = Path('dir/big-file.txt')
            (tdp / p.parent).mkdir(parents=True, exist_ok=True)
            digest = "abc123abc123/343"
            rrpl = tdp / 'action_log.rrpl'
            rrpl_contents = f"""
command: {{
}}
remote_metadata: {{
  action_digest: "feedfacefeedface/1337"
  output_file_digests: {{
    key: "{p}"
    value: "{digest}"
  }}
}}
"""
            _write_file_contents(rrpl, rrpl_contents)
            build_id = 'xyzzy'
            log_record = remote_action.ReproxyLogEntry.parse_action_log(rrpl)
            log_record.make_download_stubs(
                files=[p], dirs=[], working_dir_abs=tdp, build_id=build_id)

            destination = tdp / p
            self.assertTrue(remote_action.is_download_stub_file(destination))

            exec_root = '/exec/root'
            with mock.patch.object(
                    remotetool.RemoteTool, 'download_blob',
                    return_value=cl_utils.SubprocessResult(0)) as mock_download:
                with mock.patch.object(Path, 'rename') as mock_rename:
                    remote_action.download_from_stub(
                        destination,
                        downloader=self.downloader,
                        working_dir_abs=tdp)
            mock_download.assert_called_with(
                path=Path(str(destination) + '.download-tmp'),
                digest=digest,
                cwd=tdp,
            )
            mock_rename.assert_called_with(destination)

    def test_create_directory_stub_and_download(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            p = Path('bag/of/goodies')
            (tdp / p.parent).mkdir(parents=True, exist_ok=True)
            digest = "09ab9c86d8f001a/6540"
            rrpl = tdp / 'action_log-2.rrpl'
            rrpl_contents = f"""
command: {{
}}
remote_metadata: {{
  action_digest: "cafef00dcafef00d/656"
  output_directory_digests: {{
    key: "{p}"
    value: "{digest}"
  }}
}}
"""
            _write_file_contents(rrpl, rrpl_contents)
            build_id = 'yzzyx'
            log_record = remote_action.ReproxyLogEntry.parse_action_log(rrpl)
            log_record.make_download_stubs(
                files=[], dirs=[p], working_dir_abs=tdp, build_id=build_id)

            destination = tdp / p
            self.assertTrue(remote_action.is_download_stub_file(destination))

            exec_root = '/exec/root'
            with mock.patch.object(
                    remotetool.RemoteTool, 'download_dir',
                    return_value=cl_utils.SubprocessResult(0)) as mock_download:
                with mock.patch.object(Path, 'rename') as mock_rename:
                    remote_action.download_from_stub(
                        destination,
                        downloader=self.downloader,
                        working_dir_abs=tdp)
            mock_download.assert_called_with(
                path=Path(str(destination) + '.download-tmp'),
                digest=digest,
                cwd=tdp,
            )
            mock_rename.assert_called_with(destination)

    def test_read_fail(self):
        with tempfile.TemporaryDirectory() as td:
            stub_file = Path(td) / 'testing.stub'
            _write_file_contents(stub_file, '#!/bin/sh\nnot a stub file\n')
            with self.assertRaises(remote_action.DownloadStubFormatError):
                remote_action.DownloadStubInfo.read_from_file(stub_file)

    def test_stub_write_read_match(self):
        stub = remote_action.DownloadStubInfo(
            path=Path('foo/bar.baz'),
            type="file",
            blob_digest="abc00101ef/240",
            action_digest="08871bc3d1/18",
            build_id="random-id888",
        )
        with tempfile.TemporaryDirectory() as td:
            stub_file = Path(td) / 'identity.stub'
            stub._write(stub_file)
            new_stub = remote_action.DownloadStubInfo.read_from_file(stub_file)

        self.assertEqual(stub, new_stub)

    def test_create(self):
        path = Path('foo/goes/deeper/bar.baz')
        stub = remote_action.DownloadStubInfo(
            path=path,
            type="file",
            blob_digest="abc00101ef/240",
            action_digest="08871bc3d1/18",
            build_id="random-id888",
        )
        with tempfile.TemporaryDirectory() as td:
            full_path = td / path
            stub.create(working_dir_abs=Path(td))
            self.assertTrue(remote_action.is_download_stub_file(full_path))
            read_back = remote_action.DownloadStubInfo.read_from_file(full_path)
            self.assertEqual(read_back, stub)

    def test_download_fail(self):
        stub = remote_action.DownloadStubInfo(
            path=Path('foo/bar.baz'),
            type="file",
            blob_digest="abcef8712/24",
            action_digest="0923d1/21",
            build_id="random-id999",
        )
        download_status = 1
        with mock.patch.object(remotetool.RemoteTool, 'download_blob',
                               return_value=cl_utils.SubprocessResult(
                                   download_status)) as mock_download:
            with mock.patch.object(Path, 'rename') as mock_rename:
                status = stub.download(
                    downloader=self.downloader,
                    working_dir_abs=Path('/root/work'))
        self.assertEqual(status.returncode, download_status)
        mock_download.assert_called_once()
        mock_rename.assert_not_called()

    def test_made_download_stubs_for_remote_execution(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = 'out.out'
        main_args, other = p.parse_known_args([download_option, '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[Path(output)],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.932874'
        fake_log_record = FakeReproxyLogEntry(completion_status='SUCCESS')
        with mock.patch.object(remote_action, '_reproxy_log_dir',
                               return_value=logdir) as mock_log_dir:
            with mock.patch.object(
                    remote_action.ReproxyLogEntry, 'parse_action_log',
                    return_value=fake_log_record) as mock_parse_log:
                with mock.patch.object(remote_action.ReproxyLogEntry,
                                       'make_download_stubs') as mock_stub:
                    with mock.patch.object(
                            remote_action.RemoteAction, '_run_maybe_remotely',
                            return_value=cl_utils.SubprocessResult(
                                0)) as mock_run:
                        with mock.patch.object(remote_action.RemoteAction,
                                               'downloader', return_value=self.
                                               downloader) as mock_downloader:
                            exit_code = action.run()
            self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_log_dir.assert_called_once()
        mock_parse_log.assert_called_with(Path(output + '.rrpl'))
        mock_stub.assert_called_with(
            files=[Path(output)],
            dirs=[],
            working_dir_abs=working_dir,
            build_id=logdir,
        )
        mock_downloader.assert_called_with()

    def test_made_download_stubs_for_racing_remote_win(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = 'out.out'
        main_args, other = p.parse_known_args(
            [download_option, '--exec_strategy=racing', '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[Path(output)],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.932874'
        fake_log_record = FakeReproxyLogEntry(
            completion_status='STATUS_CACHE_HIT')
        with mock.patch.object(remote_action, '_reproxy_log_dir',
                               return_value=logdir) as mock_log_dir:
            with mock.patch.object(
                    remote_action.ReproxyLogEntry, 'parse_action_log',
                    return_value=fake_log_record) as mock_parse_log:
                with mock.patch.object(remote_action.ReproxyLogEntry,
                                       'make_download_stubs') as mock_stub:
                    with mock.patch.object(
                            remote_action.RemoteAction, '_run_maybe_remotely',
                            return_value=cl_utils.SubprocessResult(
                                0)) as mock_run:
                        with mock.patch.object(remote_action.RemoteAction,
                                               'downloader', return_value=self.
                                               downloader) as mock_downloader:
                            exit_code = action.run()
            self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_log_dir.assert_called_once()
        mock_parse_log.assert_called_with(Path(output + '.rrpl'))
        mock_stub.assert_called_with(
            files=[Path(output)],
            dirs=[],
            working_dir_abs=working_dir,
            build_id=logdir,
        )
        mock_downloader.assert_called_with()

    def test_no_download_stubs_for_local_execution(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = 'out.out'
        main_args, other = p.parse_known_args(
            [download_option, '--exec_strategy=local', '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[Path(output)],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.888222'
        fake_log_record = FakeReproxyLogEntry(
            completion_status='STATUS_LOCAL_EXECUTION')
        with mock.patch.object(remote_action.ReproxyLogEntry,
                               'parse_action_log',
                               return_value=fake_log_record) as mock_parse_log:
            with mock.patch.object(remote_action.ReproxyLogEntry,
                                   'make_download_stubs') as mock_stub:
                with mock.patch.object(
                        remote_action.RemoteAction, '_run_maybe_remotely',
                        return_value=cl_utils.SubprocessResult(0)) as mock_run:
                    exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_parse_log.assert_called_with(Path(output + '.rrpl'))
        mock_stub.assert_not_called()

    def test_no_download_stubs_for_racing_local_win(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = 'out.out'
        main_args, other = p.parse_known_args(
            [download_option, '--exec_strategy=racing', '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[Path(output)],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.213123'
        fake_log_record = FakeReproxyLogEntry(
            completion_status='STATUS_RACING_LOCAL')
        with mock.patch.object(remote_action.ReproxyLogEntry,
                               'parse_action_log',
                               return_value=fake_log_record) as mock_parse_log:
            with mock.patch.object(remote_action.ReproxyLogEntry,
                                   'make_download_stubs') as mock_stub:
                with mock.patch.object(
                        remote_action.RemoteAction, '_run_maybe_remotely',
                        return_value=cl_utils.SubprocessResult(0)) as mock_run:
                    exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_parse_log.assert_called_with(Path(output + '.rrpl'))
        mock_stub.assert_not_called()

    def test_no_download_stubs_for_local_fallback(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = 'out.out'
        main_args, other = p.parse_known_args(
            [download_option, '--exec_strategy=remote_local_fallback', '--'] +
            command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[Path(output)],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.81891'
        fake_log_record = FakeReproxyLogEntry(
            completion_status='STATUS_LOCAL_FALLBACK')
        with mock.patch.object(remote_action.ReproxyLogEntry,
                               'parse_action_log',
                               return_value=fake_log_record) as mock_parse_log:
            with mock.patch.object(remote_action.ReproxyLogEntry,
                                   'make_download_stubs') as mock_stub:
                with mock.patch.object(
                        remote_action.RemoteAction, '_run_maybe_remotely',
                        return_value=cl_utils.SubprocessResult(0)) as mock_run:
                    exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_parse_log.assert_called_with(Path(output + '.rrpl'))
        mock_stub.assert_not_called()

    def test_explicit_always_download(self):
        exec_root = Path('/home/project')
        build_dir = Path('build-out')
        working_dir = exec_root / build_dir
        download_option = '--download_outputs=false'
        p = remote_action._MAIN_ARG_PARSER
        command = ['echo']
        output = Path('out.out')
        output_digest = '5a5a57a76a6e4e4/91'
        main_args, other = p.parse_known_args(
            [download_option, f'--download={output}', '--'] + command)
        action = remote_action.remote_action_from_args(
            main_args,
            remote_options=other,
            exec_root=exec_root,
            working_dir=working_dir,
            output_files=[output],
        )
        self.assertEqual(action.local_only_command, command)
        self.assertFalse(action.download_outputs)
        self.assertEqual(action.always_download, [output])
        options = action.options
        self.assertIn(download_option, options)
        logdir = '/fake/tmp/rpl/logz.12387127'
        action_digest = '9182731aef9ad0'
        log_record = FakeReproxyLogEntry(
            execution_id='000-111-22',
            action_digest=action_digest,
            output_file_digests={output: output_digest},
            output_directory_digests={},
            completion_status='STATUS_REMOTE_EXECUTION',
        )
        with mock.patch.object(remote_action, '_reproxy_log_dir',
                               return_value=logdir) as mock_log_dir:
            with mock.patch.object(remote_action.ReproxyLogEntry,
                                   'parse_action_log',
                                   return_value=log_record) as mock_parse_log:
                with mock.patch.object(remote_action.DownloadStubInfo,
                                       'create') as mock_write_stub:
                    with mock.patch.object(remote_action.DownloadStubInfo,
                                           'download') as mock_download:
                        with mock.patch.object(
                                remote_action.RemoteAction,
                                '_run_maybe_remotely',
                                return_value=cl_utils.SubprocessResult(
                                    0)) as mock_run:
                            with mock.patch.object(remote_action.RemoteAction,
                                                   'downloader',
                                                   return_value=self.downloader
                                                  ) as mock_downloader:
                                exit_code = action.run()
        self.assertEqual(exit_code, 0)
        mock_run.assert_called()
        mock_log_dir.assert_called_with()
        mock_write_stub.assert_called_with(working_dir)
        mock_parse_log.assert_called_with(Path(str(output) + '.rrpl'))
        mock_downloader.assert_called_with()
        mock_download.assert_called_with(
            downloader=self.downloader,
            working_dir_abs=working_dir,
        )

    def test_explicit_always_download_with_real_proxy_logdir(self):
        with tempfile.TemporaryDirectory() as td:
            exec_root = Path(td) / 'exec_root'
            logdir = Path(td) / 'tmp/reproxy/logs'
            build_dir = Path('build-out')
            working_dir = exec_root / build_dir
            download_option = '--download_outputs=false'
            p = remote_action._MAIN_ARG_PARSER
            command = ['compilezor']
            output = Path('objdir/hello-w0rld.obj')
            output_digest = '3333377777ggggcccccaaaaa/717'
            main_args, other = p.parse_known_args(
                [download_option, f'--download={output}', '--'] + command)
            action = remote_action.remote_action_from_args(
                main_args,
                remote_options=other,
                exec_root=exec_root,
                working_dir=working_dir,
                output_files=[output],
            )
            self.assertEqual(action.local_only_command, command)
            self.assertFalse(action.download_outputs)
            self.assertEqual(action.always_download, [output])
            options = action.options
            self.assertIn(download_option, options)
            action_log = Path(str(output) + '.rrpl')
            self.assertIn(
                f'--action_log={action_log}',
                set(action._generate_remote_command_prefix()))
            action_log_contents = f"""
command:  {{
        identifiers:  {{
                command_id:  "2b2b2b2b2-111111"
                invocation_id:  "26363673-4643-7789-afad-7d2f8531ba98"
                tool_name:  "re-client"
                execution_id:  "fedefedefefdedefefe"
        }}
}}
remote_metadata:  {{
        command_digest:  "f9d024e5dc99433b08f4592309379e611461498d1480e2057c7afc285d30f097/181"
        action_digest:  "87bacb7fege6ha65a3300/77"
        output_file_digests: {{
                key: "{output}"
                value: "{output_digest}"
        }}
}}
completion_status: STATUS_CACHE_HIT
"""

            def fake_run_remote(
                unused_action: remote_action.RemoteAction
            ) -> cl_utils.SubprocessResult:
                output.parent.mkdir(parents=True, exist_ok=True)
                _write_file_contents(output, 'death-to-bash-scripts\n')
                _write_file_contents(action_log, action_log_contents)
                return cl_utils.SubprocessResult(0)

            with mock.patch.object(remote_action, '_reproxy_log_dir',
                                   return_value=logdir) as mock_log_dir:
                with mock.patch.object(remote_action.DownloadStubInfo,
                                       'download') as mock_download:
                    with mock.patch(
                            'remote_action.RemoteAction._run_maybe_remotely',
                            new=fake_run_remote) as mock_run:
                        with mock.patch.object(remote_action.RemoteAction,
                                               'downloader', return_value=self.
                                               downloader) as mock_downloader:
                            exit_code = action.run()
            self.assertEqual(exit_code, 0)
            mock_log_dir.assert_called_once()
            mock_downloader.assert_called_with()
            mock_download.assert_called_with(
                downloader=self.downloader,
                working_dir_abs=working_dir,
            )


class RbeDiagnosticsTests(unittest.TestCase):

    def _make_remote_action(self, **kwargs):
        command = ['echo', 'hello']
        exec_root = Path('/path/to/project/root')
        working_dir = exec_root / 'build/stuff/here'
        return remote_action.RemoteAction(
            rewrapper='/path/to/rewrapper',
            command=command,
            exec_root=exec_root,
            working_dir=working_dir,
            **kwargs,
        )

    def test_analyze_conditions_positive(self):
        action = self._make_remote_action(diagnose_nonzero=True)

        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(1)) as mock_run:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                with mock.patch.object(remote_action,
                                       'analyze_rbe_logs') as mock_analyze:
                    self.assertEqual(action.run(), 1)

        mock_cleanup.assert_called_once()
        mock_run.assert_called_once()
        mock_analyze.assert_called_once()
        args, kwargs = mock_analyze.call_args_list[0]
        self.assertEqual(kwargs["action_log"], action._action_log)

    def test_analyzing_not_requested(self):
        action = self._make_remote_action(diagnose_nonzero=False)

        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(1)) as mock_run:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                with mock.patch.object(remote_action,
                                       'analyze_rbe_logs') as mock_analyze:
                    self.assertEqual(action.run(), 1)

        mock_cleanup.assert_called_once()
        mock_run.assert_called_once()
        mock_analyze.assert_not_called()

    def test_not_analyzing_on_success(self):
        action = self._make_remote_action(diagnose_nonzero=True)

        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(0)) as mock_run:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                with mock.patch.object(remote_action,
                                       'analyze_rbe_logs') as mock_analyze:
                    self.assertEqual(action.run(), 0)

        mock_cleanup.assert_called_once()
        mock_run.assert_called_once()
        mock_analyze.assert_not_called()

    def test_not_analyzing_local_execution(self):
        action = self._make_remote_action(
            diagnose_nonzero=True,
            exec_strategy="local",
        )

        with mock.patch.object(
                remote_action.RemoteAction, '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(1)) as mock_run:
            with mock.patch.object(remote_action.RemoteAction,
                                   '_cleanup') as mock_cleanup:
                with mock.patch.object(remote_action,
                                       'analyze_rbe_logs') as mock_analyze:
                    self.assertEqual(action.run(), 1)

        mock_cleanup.assert_called_once()
        mock_run.assert_called_once()
        mock_analyze.assert_not_called()

    def test_analyze_flow(self):
        pid = 6789
        action_log = Path('obj/my_action.rrpl')
        fake_rewrapper_logs = [
            f'/noisy/log/rewrapper.where.who.log.INFO.when.{pid}',
            f'/noisy/log/rewrapper.where.who.log.ERROR.when.{pid}',
        ]
        unnamed_mocks = [
            mock.patch.object(
                remote_action,
                '_reproxy_log_dir',
                return_value='/path/to/tmp/reproxy.999999'),
            mock.patch.object(
                remote_action,
                '_rewrapper_log_dir',
                return_value='/path/to/tmp/reproxy.999999/wrapper/logz'),
            mock.patch.object(Path, 'is_file', return_value=True),
        ]
        with contextlib.ExitStack() as stack:
            for m in unnamed_mocks:
                stack.enter_context(m)

            with mock.patch.object(
                    Path, 'glob',
                    return_value=fake_rewrapper_logs) as mock_glob:
                with mock.patch.object(
                        remote_action.ReproxyLogEntry,
                        'parse_action_log') as mock_parse_action_log:
                    with mock.patch.object(remote_action,
                                           '_file_lines_matching',
                                           return_value=['this is interesting'
                                                        ]) as mock_read_log:
                        with mock.patch.object(remote_action,
                                               '_diagnose_reproxy_error_line'
                                              ) as mock_diagnose_line:
                            remote_action.analyze_rbe_logs(
                                rewrapper_pid=pid,
                                action_log=action_log,
                            )
        mock_glob.assert_called_once()
        mock_read_log.assert_called_once()
        mock_parse_action_log.assert_called_with(action_log)
        mock_diagnose_line.assert_called()

    def test_parse_reproxy_log_record_lines(self):
        exec_id = "xx-yy-zzzz"
        action_digest = "2afd98ae7274456b2bfc208e10f4cbe75fca88c2c41e352e57cb6b9ad840bf64/144"
        output_path = Path("obj/sub/lib/foo.o")
        output_digest = "345324aefg983bc0/531"
        log_lines = f"""
command:  {{
        identifiers:  {{
                command_id:  "8ea55c85-0ae36078"
                invocation_id:  "979eedda-4643-45da-af15-0d2f8531ba98"
                tool_name:  "re-client"
                execution_id:  "{exec_id}"
        }}
}}
remote_metadata:  {{
        command_digest:  "e9d024e5dc99438b08f4592a09379e65146149821480e2057c7afc285d30f090/181"
        action_digest:  "{action_digest}"
        output_file_digests: {{
                key: "{output_path}"
                value: "{output_digest}"
        }}
}}
        """.splitlines()
        log_entry = remote_action.ReproxyLogEntry._parse_lines(log_lines)
        self.assertEqual(log_entry.execution_id, exec_id)
        self.assertEqual(log_entry.action_digest, action_digest)
        self.assertEqual(
            log_entry.output_file_digests[output_path], output_digest)

    def test_diagnose_uninteresting_log_line(self):
        line = "This diagnostic does not appear interesting."
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            remote_action._diagnose_reproxy_error_line(line)
        self.assertEqual(f.getvalue(), '')

    def test_diagnose_fail_to_dial(self):
        line = "Fail to dial something something unix:///path/to/reproxy.socket"
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            remote_action._diagnose_reproxy_error_line(line)
        self.assertIn("reproxy is not running", f.getvalue())

    def test_diagnose_rbe_permissions(self):
        line = "Error connecting to remote execution client: rpc error: code = PermissionDenied.  You have no power here!"
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            remote_action._diagnose_reproxy_error_line(line)
        self.assertIn(
            "You might not have permssion to access the RBE instance",
            f.getvalue())

    def test_diagnose_missing_input_file(self):
        path = "../oops/did/I/forget/this.file"
        line = f"Status:LocalErrorResultStatus ... Err:stat {path}: no such file or directory"
        f = io.StringIO()
        with contextlib.redirect_stdout(f):
            remote_action._diagnose_reproxy_error_line(line)
        self.assertIn(
            f"missing a local input file for uploading: {path} (source)",
            f.getvalue())


class MainTests(unittest.TestCase):

    def test_help_flag(self):
        stdout = io.StringIO()
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    remote_action.main(['--help'])
        mock_exit.assert_called_with(0)

    def test_auto_relaunch_with_reproxy_not_needed_for_local(self):
        command = ['--local', '--', 'echo', 'hello']
        exit_code = 7
        with mock.patch.object(remote_action.RemoteAction, 'run_with_main_args',
                               return_value=exit_code):
            self.assertEqual(remote_action.main(command), exit_code)

    def test_auto_relaunch_with_reproxy_not_needed_for_dry_run(self):
        command = ['--dry-run', '--', 'echo', 'hello']
        self.assertEqual(remote_action.main(command), 0)

    def test_auto_relaunch_with_reproxy_not_needed_with_env(self):
        command = ['--', 'echo', 'hello']
        exit_code = 9
        with mock.patch.object(os.environ, 'get',
                               return_value='/any/value/will/do') as mock_env:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=exit_code) as mock_run:
                self.assertEqual(remote_action.main(command), exit_code)
        mock_env.assert_called()
        mock_run.assert_called_once()

    def test_auto_relaunch_with_reproxy_needed(self):
        command = ['--', 'echo', 'hello']
        # Expect to relaunch because the necessary env variables
        # are absent.
        with mock.patch.object(os.environ, 'get',
                               return_value=None) as mock_env:
            with mock.patch.object(cl_utils, 'exec_relaunch',
                                   side_effect=ImmediateExit) as mock_relaunch:
                with self.assertRaises(ImmediateExit):
                    remote_action.main(command)
        mock_relaunch.assert_called_once()
        args, kwargs = mock_relaunch.call_args_list[0]
        relaunch_cmd = args[0]
        self.assertEqual(relaunch_cmd[0], fuchsia.REPROXY_WRAP)
        cmd_slices = cl_utils.split_into_subsequences(relaunch_cmd[1:], '--')
        reproxy_args, self_script, wrapped_command = cmd_slices
        self.assertEqual(reproxy_args, [])
        self.assertIn('python', self_script[0])
        self.assertTrue(self_script[-1].endswith('remote_action.py'))
        self.assertEqual(wrapped_command, command[1:])

    def test_main_args_remote_inputs(self):
        command = ['--inputs', 'src/in.txt', '--', 'echo', 'hello']
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=0) as mock_run:
                self.assertEqual(remote_action.main(command), 0)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        main_args = args[0]
        self.assertEqual(main_args.inputs, ['src/in.txt'])

    def test_main_args_remote_inputs_repeated(self):
        command = [
            '--inputs', 'src/in.txt', '--inputs=another.s', '--', 'echo',
            'hello'
        ]
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=0) as mock_run:
                self.assertEqual(remote_action.main(command), 0)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        main_args = args[0]
        self.assertEqual(main_args.inputs, ['src/in.txt', 'another.s'])

    def test_main_args_local(self):
        command = ['--local', '--', 'echo', 'hello']
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=0) as mock_run:
                self.assertEqual(remote_action.main(command), 0)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        main_args = args[0]
        self.assertTrue(main_args.local)

    def test_flag_forwarding_remote_disable(self):
        command = ['--', 'echo', '--remote-disable', 'hello']
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=0) as mock_run:
                self.assertEqual(remote_action.main(command), 0)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        main_args = args[0]
        self.assertTrue(main_args.local)

    def test_main_args_local_check_determinism(self):
        command = ['--local', '--check-determinism', '--', 'echo', 'hello']
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(remote_action.RemoteAction,
                                   'run_with_main_args',
                                   return_value=0) as mock_run:
                self.assertEqual(remote_action.main(command), 0)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args_list[0]
        main_args = args[0]
        self.assertTrue(main_args.local)
        self.assertTrue(main_args.check_determinism)


if __name__ == '__main__':
    unittest.main()
