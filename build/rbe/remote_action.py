#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Construct and execution remote actions with rewrapper.

This script is both a library and standalone binary for
driving rewrapper.

Usage:
  $0 [remote-options...] -- command...
"""

import argparse
import os
import subprocess
import shlex

import fuchsia
import cl_utils

from typing import Callable, Iterable, Optional, Sequence

_SCRIPT_BASENAME = os.path.basename(__file__)

PROJECT_ROOT = fuchsia.project_root_dir()
PROJECT_ROOT_REL = os.path.relpath(PROJECT_ROOT, start=os.curdir)


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def relativize_to_exec_root(path: str, start=None) -> str:
    return os.path.relpath(path, start=start or PROJECT_ROOT)


class RemoteAction(object):
    """RemoteAction represents a command that is to be executed remotely."""

    def __init__(
        self,
        rewrapper: str,
        command: Sequence[str],
        options: Sequence[str] = None,
        exec_root: Optional[str] = None,
        inputs: Sequence[str] = None,
        output_files: Sequence[str] = None,
        output_dirs: Sequence[str] = None,
        save_temps: bool = False,
        auto_reproxy: bool = False,
    ):
        """RemoteAction constructor.

        Args:
          rewrapper: path to rewrapper binary
          options: rewrapper options (not already covered by other parameters)
          command: the command to execute remotely
          exec_root: an absolute path location that is parent to all of this
            remote action's inputs and outputs.
          inputs: inputs needed for remote execution, relative to the current working dir.
          output_files: files to be fetched after remote execution, relative to the
            current working dir.
          output_dirs: directories to be fetched after remote execution, relative to the
            current working dir.
          save_temps: if true, keep around temporarily generated files after execution.
          auto_reproxy: if true, launch reproxy around the rewrapper invocation.
            This is not needed if reproxy is already running.
        """
        self._rewrapper = rewrapper
        self._save_temps = save_temps
        self._auto_reproxy = auto_reproxy
        self._exec_root = exec_root or PROJECT_ROOT  # absolute path
        # Parse and strip out --remote-* flags from command.
        remote_args, self._remote_command = _REMOTE_FLAG_ARG_PARSER.parse_known_args(
            command)
        self._remote_disable = remote_args.disable
        self._options = (options or []) + remote_args.flags
        # Inputs and outputs parameters are relative to current working dir,
        # but they will be relativized to exec_root for rewrapper.
        # It is more natural to copy input/output paths that are relative to the
        # current working directory.
        self._inputs = (inputs or []) + list(
            cl_utils.flatten_comma_list(remote_args.inputs))
        self._output_files = (output_files or []) + list(
            cl_utils.flatten_comma_list(remote_args.output_files))
        self._output_dirs = (output_dirs or []) + list(
            cl_utils.flatten_comma_list(remote_args.output_dirs))
        self._cleanup_files = []

    @property
    def exec_root(self) -> str:
        return self._exec_root

    @property
    def local_command(self) -> Sequence[str]:
        """This is the original command that would have been run locally.
        All of the --remote-* flags have been removed at this point.
        """
        return self._remote_command

    @property
    def options(self) -> Sequence[str]:
        return self._options

    @property
    def auto_reproxy(self) -> bool:
        return self._auto_reproxy

    @property
    def save_temps(self) -> bool:
        return self._save_temps

    @property
    def remote_disable(self) -> bool:
        return self._remote_disable

    def _relativize_to_exec_root(self, paths: Sequence[str]) -> Sequence[str]:
        current_dir_abs = os.path.abspath(os.curdir)
        return [
            relativize_to_exec_root(
                os.path.normpath(os.path.join(current_dir_abs, path)),
                start=self._exec_root) for path in paths
        ]

    @property
    def build_subdir(self) -> str:
        """This is the relative path from the exec_root to the current working dir."""
        return os.path.relpath(os.curdir, start=self._exec_root)

    @property
    def inputs_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_to_exec_root(self._inputs)

    @property
    def output_files_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_to_exec_root(self._output_files)

    @property
    def output_dirs_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_to_exec_root(self._output_dirs)

    def _inputs_list_file(self) -> str:
        inputs_list_file = self._output_files[0] + '.inputs'
        with open(inputs_list_file, 'w') as f:
            for i in self.inputs_relative_to_project_root:
                f.write(i + '\n')
        return inputs_list_file

    def _generate_rewrapper_command_prefix(self) -> Iterable[str]:
        yield self._rewrapper
        yield f"--exec_root={self._exec_root}"
        yield from self._options

        if self._inputs:
            # TODO(http://fxbug.dev/124186): use --input_list_paths only if list is sufficiently long
            inputs_list_file = self._inputs_list_file()
            self._cleanup_files.append(inputs_list_file)
            yield f"--input_list_paths={inputs_list_file}"

        # outputs (files and dirs) need to be relative to the exec_root,
        # even as we run from inside the build_dir under exec_root.
        if self._output_files:
            output_files = ','.join(self.output_files_relative_to_project_root)
            yield f"--output_files={output_files}"

        if self._output_dirs:
            output_dirs = ','.join(self.output_dirs_relative_to_project_root)
            yield f"--output_directories={output_dirs}"

    def _generate_command(self) -> Iterable[str]:
        """Generates the rewrapper command, one token at a time."""
        if not self._remote_disable:
            # TODO(http://fxbug.dev/124190): detect that reproxy is needed, by checking the environment
            if self._auto_reproxy:
                yield fuchsia.REPROXY_WRAP
                yield '--'

            yield from self._generate_rewrapper_command_prefix()
            yield '--'

        yield from self._remote_command

    @property
    def command(self) -> Sequence[str]:
        """This is the fully constructed rewrapper command executed on the host."""
        return list(self._generate_command())

    # features to port over from fuchsia-rbe-action.sh:
    # TODO(http://fxbug.dev/96250): implement fsatrace mutator
    # TODO(http://fxbug.dev/96250): implement remote_log mutator
    # TODO(http://fxbug.dev/123178): facilitate delayed downloads using --action_log

    def _cleanup(self):
        for f in self._cleanup_files:
            os.remove(f)

    def run(self) -> int:
        """Remotely execute the command.

        Returns:
          rewrapper's exit code, which is the remote execution exit code in most cases,
            but sometimes an re-client internal error code like 35 or 45.
        """
        try:
            # TODO(http://fxbug.dev/96250): handle some re-client error cases
            #   and in some cases, retry once
            return subprocess.call(self.command)
        finally:
            if not self._save_temps:
                self._cleanup()


def _remote_flag_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=
        "Extracts flags that are intended for the remote execution wrapper from a local command.  This allows control of remote execution behavior in a otherwise remote-execution-oblivious command.  All flags start with '--remote'.  Call `parse_known_args()` to remove these remote flags from the rest of the command.",
        argument_default=[],
    )
    parser.add_argument(
        "--remote-disable",
        dest='disable',
        action="store_true",
        default=False,
        help="Disable remote execution, run the original command locally.",
    )
    parser.add_argument(
        "--remote-inputs",
        dest='inputs',
        action='append',
        default=[],
        help=
        "Specify additional remote inputs, relative to the current working dir.",
    )
    parser.add_argument(
        "--remote-outputs",  # TODO: rename this to --remote-output-files
        dest='output_files',
        action='append',
        default=[],
        help="Specify additional remote output files, relative to the current working dir (repeatable).",
    )
    parser.add_argument(
        "--remote-output-dirs",
        action='append',
        dest='output_dirs',
        default=[],
        help=
        "Specify additional remote output directories, relative to the current working dir (repeatable).",
    )
    parser.add_argument(
        "--remote-flag",
        action='append',
        dest='flags',
        default=[],
        help="Forward these flags to the rewrapper (repeatable).",
    )
    return parser


_REMOTE_FLAG_ARG_PARSER = _remote_flag_arg_parser()


def inherit_main_arg_parser_flags(
    parser: argparse.ArgumentParser,
    default_cfg: str = None,
    default_bindir: str = None,
):
    """Extend an existing argparser with standard flags.

    These flags are available for tool-specific remote command wrappers to use.
    """
    default_cfg = default_cfg or os.path.join(
        PROJECT_ROOT_REL, 'build', 'rbe', 'fuchsia-rewrapper.cfg')
    default_bindir = default_bindir or os.path.join(
        PROJECT_ROOT_REL, fuchsia.RECLIENT_BINDIR)
    group = parser.add_argument_group("Generic remote action options")
    group.add_argument(
        "--cfg",
        type=str,
        default=default_cfg,
        help="rewrapper config file.",
    )
    group.add_argument(
        "--bindir",
        type=str,
        default=default_bindir,
        help="Path to reclient tools like rewrapper, reproxy.",
    )
    group.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="Show final rewrapper command and exit.",
    )
    group.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Print additional debug information while running.",
    )
    group.add_argument(
        "--label",
        type=str,
        default="",
        help="Build system identifier, for diagnostic messages",
    )
    group.add_argument(
        "--log",
        type=str,
        dest="remote_log",
        default="",
        nargs='?',
        help="Capture remote execution's stdout/stderr to a log file.",
    )
    group.add_argument(
        "--save-temps",
        action="store_true",
        default=False,
        help="Keep around intermediate files that are normally cleaned up.",
    )
    group.add_argument(
        "--auto-reproxy",
        action="store_true",
        default=False,
        help="Startup and shutdown reproxy around the rewrapper invocation.",
    )
    # Positional args are the command and arguments to run.
    parser.add_argument(
        "command", nargs="*", help="The command to run remotely")


def _main_arg_parser() -> argparse.ArgumentParser:
    """Construct the argument parser, called by main()."""
    parser = argparse.ArgumentParser(
        description="Executes a build action command remotely.",
        argument_default=[],
    )
    inherit_main_arg_parser_flags(parser)
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def remote_action_from_args(
        main_args: argparse.Namespace,
        remote_options: Sequence[str] = None,
        command: Sequence[str] = None,
        **kwargs,  # other RemoteAction __init__ params
) -> RemoteAction:
    """Construct a remote action based on argparse parameters."""
    return RemoteAction(
        rewrapper=os.path.join(main_args.bindir, "rewrapper"),
        options=['--cfg', main_args.cfg] + (remote_options or []),
        command=command or main_args.command,
        save_temps=main_args.save_temps,
        auto_reproxy=main_args.auto_reproxy,
        **kwargs,
    )


def main(argv: Sequence[str]) -> None:
    main_args, other_remote_options = _MAIN_ARG_PARSER.parse_known_args(argv)
    # forward all unknown flags to rewrapper
    # forwarded rewrapper options with values must be written as '--flag=value',
    # not '--flag value' because argparse doesn't know what unhandled flags
    # expect values.

    remote_action = remote_action_from_args(
        main_args=main_args, remote_options=other_remote_options)

    command_str = ' '.join(shlex.quote(t) for t in remote_action.command)
    if main_args.verbose and not main_args.dry_run:
        msg(command_str)
    if main_args.dry_run:
        label_str = " "
        if main_args.label:
            label_str += f"[{main_args.label}] "
        msg(f"[dry-run only]{label_str}{command_str}")
        return 0

    return remote_action.run()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
