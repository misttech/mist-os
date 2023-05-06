#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
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

import prebuilt_tool_remote_wrapper

import cl_utils
import fuchsia
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


class PrebuiltToolActionTests(unittest.TestCase):

    def test_host_remote_same(self):
        fake_root = Path('/home/project')
        fake_builddir = Path('out/really-not-default')
        fake_cwd = fake_root / fake_builddir
        local_tool = Path('../../path/to/hammer')  # platform-independent
        source = Path('nail.steel')
        output = Path('furniture.obj')
        command = _strs([local_tool, '-i', source, '-o', output])
        c = prebuilt_tool_remote_wrapper.PrebuiltToolAction(
            [f'--inputs={source}', f'--output_files={output}', '--'] + command,
            exec_root=fake_root,
            working_dir=fake_cwd,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.local_tool, local_tool)
        self.assertEqual(c.remote_tool, local_tool)
        self.assertEqual(c.command_line_inputs, [source])
        self.assertEqual(c.command_line_output_files, [output])
        self.assertEqual(c.command_line_output_dirs, [])
        self.assertEqual(c.local_command, command)
        self.assertEqual(c.remote_command, command)
        self.assertFalse(c.local_only)

        with mock.patch.object(prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                               'check_preconditions') as mock_check:
            self.assertIsNone(c.prepare())
            self.assertEqual(
                set(c.remote_action.inputs_relative_to_project_root),
                {fake_builddir / source,
                 Path('path/to/hammer')})
            with mock.patch.object(
                    prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                    '_run_remote_action', return_value=0) as mock_call:
                exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_once()

    def test_host_remote_different(self):
        fake_root = Path('/home/project')
        fake_builddir = Path('out/really-not-default')
        fake_cwd = fake_root / fake_builddir
        local_tool = Path(
            '../../path/mac-arm64/bin/screwdriver')  # platform-specific
        remote_tool = Path(
            f'../../path/{fuchsia.REMOTE_PLATFORM}/bin/screwdriver'
        )  # platform-specific
        source = Path('nail.steel')
        output = Path('furniture.obj')
        command = _strs([local_tool, '-i', source, '-o', output])
        c = prebuilt_tool_remote_wrapper.PrebuiltToolAction(
            [f'--inputs={source}', f'--output_files={output}', '--'] + command,
            exec_root=fake_root,
            working_dir=fake_cwd,
            host_platform='mac-arm64',  # host != remote exec
            auto_reproxy=False,
        )
        self.assertFalse(c.verbose)
        self.assertFalse(c.dry_run)
        self.assertEqual(c.local_tool, local_tool)
        self.assertEqual(c.remote_tool, remote_tool)
        self.assertEqual(c.command_line_inputs, [source])
        self.assertEqual(c.command_line_output_files, [output])
        self.assertEqual(c.command_line_output_dirs, [])
        self.assertEqual(c.local_command, command)
        self.assertEqual(c.remote_command, command)
        self.assertFalse(c.local_only)

        with mock.patch.object(prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                               'check_preconditions') as mock_check:
            self.assertIsNone(c.prepare())
            self.assertEqual(
                set(c.remote_action.inputs_relative_to_project_root), {
                    fake_builddir / source,
                    Path(f'path/{fuchsia.REMOTE_PLATFORM}/bin/screwdriver')
                })
            with mock.patch.object(
                    prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                    '_run_remote_action', return_value=0) as mock_call:
                exit_code = c.run()
        self.assertEqual(exit_code, 0)
        mock_call.assert_called_once()

    def test_remote_failure(self):
        fake_root = Path('/home/project')
        fake_builddir = Path('out/really-not-default')
        fake_cwd = fake_root / fake_builddir
        local_tool = Path('../../path/to/hammer')  # platform-independent
        command = _strs([local_tool])
        c = prebuilt_tool_remote_wrapper.PrebuiltToolAction(
            ['--'] + command,
            exec_root=fake_root,
            working_dir=fake_cwd,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )

        mock_exit_code = 2
        with mock.patch.object(prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                               'check_preconditions') as mock_check:
            self.assertIsNone(c.prepare())
            with mock.patch.object(
                    prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                    '_run_remote_action',
                    return_value=mock_exit_code) as mock_call:
                exit_code = c.run()
        self.assertEqual(exit_code, mock_exit_code)
        mock_call.assert_called_once()

    def test_remote_flag_back_propagating(self):
        tool = Path('path/to/drill')
        flag = '--foo-bar'
        command = _strs([
            tool,
            f'--remote-flag={flag}',
        ])
        filtered_command = _strs([tool])
        c = prebuilt_tool_remote_wrapper.PrebuiltToolAction(
            ['--'] + command,
            host_platform=fuchsia.REMOTE_PLATFORM,  # host = remote exec
            auto_reproxy=False,
        )

        with mock.patch.object(prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                               'check_preconditions') as mock_check:
            self.assertIsNone(c.prepare())
        # check that rewrapper option sees --foo=bar
        remote_action_command = c.remote_action.launch_command
        prefix, sep, wrapped_command = cl_utils.partition_sequence(
            remote_action_command, '--')
        self.assertIn(flag, prefix)
        self.assertEqual(wrapped_command, filtered_command)


class MainTests(unittest.TestCase):

    def test_help_implicit(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    prebuilt_tool_remote_wrapper.main([])
        mock_exit.assert_called_with(0)

    def test_help_flag(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    prebuilt_tool_remote_wrapper.main(['--help'])
        mock_exit.assert_called_with(0)

    def test_local_mode_forced(self):
        exit_code = 24
        with mock.patch.object(remote_action,
                               'auto_relaunch_with_reproxy') as mock_relaunch:
            with mock.patch.object(
                    prebuilt_tool_remote_wrapper.PrebuiltToolAction,
                    '_run_locally', return_value=exit_code) as mock_run:
                self.assertEqual(
                    prebuilt_tool_remote_wrapper.main(
                        [
                            '--local', '--', 'shebang', '-c', 'flu.cc', '-o',
                            'cuckoo'
                        ]), exit_code)
        mock_relaunch.assert_called_once()
        mock_run.assert_called_with()

    def test_auto_relaunched_with_reproxy(self):
        argv = ['--', 'shebang', '-c', 'flu.cc', '-o', 'cuckoo']
        with mock.patch.object(os.environ, 'get',
                               return_value=None) as mock_env:
            with mock.patch.object(cl_utils, 'exec_relaunch',
                                   side_effect=ImmediateExit) as mock_relaunch:
                with self.assertRaises(ImmediateExit):
                    prebuilt_tool_remote_wrapper.main(argv)
        mock_env.assert_called()
        mock_relaunch.assert_called_once()
        args, kwargs = mock_relaunch.call_args_list[0]
        relaunch_cmd = args[0]
        self.assertEqual(relaunch_cmd[0], fuchsia.REPROXY_WRAP)
        cmd_slices = cl_utils.split_into_subsequences(relaunch_cmd[1:], '--')
        reproxy_args, self_script, wrapped_command = cmd_slices
        self.assertEqual(reproxy_args, [])
        self.assertIn('python', self_script[0])
        self.assertTrue(
            self_script[-1].endswith('prebuilt_tool_remote_wrapper.py'))
        self.assertEqual(wrapped_command, argv[1:])


if __name__ == '__main__':
    unittest.main()
