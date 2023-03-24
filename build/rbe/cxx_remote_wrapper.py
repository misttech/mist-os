#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Remotely compile C and C++.

This script functions as a standalone executable.

Usage:
  $0 [remote options...] -- compile-comand...
"""

import argparse
import enum
import os
import shlex
import subprocess
import sys

import cxx
import fuchsia
import remote_action

from typing import Iterable, Sequence

_SCRIPT_BASENAME = os.path.basename(__file__)
_SCRIPT_DIR = os.path.dirname(__file__)


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


REMOTE_COMPILER_SWAPPER = os.path.join(
    _SCRIPT_DIR, "cxx-swap-remote-compiler.sh")


def _main_arg_parser() -> argparse.ArgumentParser:
    """Construct the argument parser, called by main()."""
    parser = argparse.ArgumentParser(
        description="Prepares a C++ command for remote execution.",
        argument_default=[],
    )
    remote_action.inherit_main_arg_parser_flags(parser)
    group = parser.add_argument_group("C++ remote action options")
    group.add_argument(
        "--cpp-strategy",
        type=str,
        choices=['auto', 'local', 'integrated'],
        default='auto',
        help="""Configure how C-preprocessing is done.
    integrated: preprocess and compile in a single step,
    local: preprocess locally, compile remotely,
    auto (default): one of the above, chosen by the script automatically.
""",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def check_missing_remote_tools(compiler_type: cxx.Compiler) -> Iterable[str]:
    required_remote_tools = []
    if compiler_type == cxx.Compiler.CLANG:
        required_remote_tools.append(fuchsia.REMOTE_CLANG_SUBDIR)
    if compiler_type == cxx.Compiler.GCC:
        required_remote_tools.append(fuchsia.REMOTE_GCC_SUBDIR)

    for d in required_remote_tools:
        if not os.path.exists(os.path.join(remote_action.PROJECT_ROOT_REL, d)):
            yield d


def _command_to_str(command: Iterable[str]) -> str:
    return ' '.join(shlex.quote(t) for t in command)


class CxxRemoteAction(object):

    def __init__(self, argv: Sequence[str], working_dir: str = None):
        self._main_args, main_remote_options = _MAIN_ARG_PARSER.parse_known_args(
            argv)
        # forward all unknown flags to rewrapper
        # forwarded rewrapper options with values must be written as '--flag=value',
        # not '--flag value' because argparse doesn't know what unhandled flags
        # expect values.

        self._working_dir = os.path.abspath(working_dir or os.curdir)
        self._cxx_action = cxx.CxxAction(command=self._main_args.command)

        if not self._cxx_action.target and self._cxx_action.compiler_is_clang:
            raise Exception(
                "For remote compiling with clang, an explicit --target is required, but is missing."
            )

        # Determine whether this action can be done remotely.
        self._local_only = False
        if self._cxx_action.sources[0].file.endswith('.S'):
            # Compiling un-preprocessed assembly is not supported remotely.
            self._local_only = True

        # check for required remote tools
        # TODO: bypass this check when remote execution is disabled.
        missing_required_tools = list(
            check_missing_remote_tools(self._cxx_action.compiler.type))
        if missing_required_tools:
            raise Exception(
                f"Missing the following tools needed for remote compiling C++: {missing_required_tools}.  See tqr/563565 for how to fetch the needed packages."
            )

        self._cpp_strategy = self._resolve_cpp_strategy()

        remote_inputs = []
        self._cleanup_files = []
        self._local_preprocess_command = None
        if self.cpp_strategy == 'local':
            # preprocess locally, then compile the result remotely
            preprocessed_source = self._cxx_action.preprocessed_output
            self._cleanup_files += [preprocessed_source]
            remote_inputs += [preprocessed_source]
            self._local_preprocess_command, remote_command = self._cxx_action.split_preprocessing(
            )
        elif self.cpp_strategy == 'integrated':
            # preprocess driven by the compiler, done remotely
            remote_command = self._cxx_action.command
            # TODO: might need -Wno-constant-logical-operand to workaround
            #   ZX_DEBUG_ASSERT.

        # Prepare remote compile action
        remote_output_dirs = []
        remote_options = [
            "--labels=type=compile,compiler=clang,lang=cpp",  # TODO: gcc?
            "--canonicalize_working_dir=true",
        ]
        # The output file is inferred automatically by rewrapper in C++ mode,
        # but naming it explicitly here makes it easier for RemoteAction
        # to use the output file name for other auxiliary files.
        remote_output_files = [self._cxx_action.output_file]

        if self._cxx_action.crash_diagnostics_dir:
            remote_output_dirs.append(self._cxx_action.crash_diagnostics_dir)

        # Support for remote cross-compilation:
        if fuchsia.HOST_PREBUILT_PLATFORM_SUBDIR != 'linux-x64':
            # compiler path is relative to current working dir
            compiler_swapper_rel = os.path.relpath(
                REMOTE_COMPILER_SWAPPER, self.working_dir)
            remote_inputs.append(self.remote_compiler)
            remote_options.append(f'--remote_wrapper={compiler_swapper_rel}')
            remote_command = [compiler_swapper_rel] + remote_command

        self._remote_action = remote_action.remote_action_from_args(
            main_args=self._main_args,
            remote_options=main_remote_options + remote_options,
            command=remote_command,
            inputs=remote_inputs,
            output_files=remote_output_files,
            output_dirs=remote_output_dirs,
            working_dir=self.working_dir,
        )

    @property
    def working_dir(self) -> str:
        return self._working_dir

    @property
    def verbose(self) -> bool:
        return self._main_args.verbose

    @property
    def dry_run(self) -> bool:
        return self._main_args.dry_run

    def _resolve_cpp_strategy(self) -> str:
        """Resolve preprocessing strategy to 'local' or 'integrated'."""
        cpp_strategy = self._main_args.cpp_strategy
        if cpp_strategy == 'auto':
            if self._cxx_action.uses_macos_sdk:
                # cannot upload Mac headers for remote compiling
                cpp_strategy = 'local'
            else:
                cpp_strategy = 'integrated'
        return cpp_strategy

    @property
    def cpp_strategy(self) -> str:
        return self._cpp_strategy

    @property
    def original_compile_command(self) -> Sequence[str]:
        return self.cxx_action.command

    @property
    def local_only(self) -> bool:
        return self._local_only

    @property
    def cxx_action(self) -> cxx.CxxAction:
        return self._cxx_action

    @property
    def local_preprocess_command(self) -> Sequence[str]:
        return self._local_preprocess_command

    @property
    def remote_compile_action(self) -> remote_action.RemoteAction:
        return self._remote_action

    @property
    def remote_compiler(self) -> str:
        return fuchsia.remote_executable(self._cxx_action.compiler.tool)

    def _run_locally(self) -> int:
        return subprocess.call(self.original_compile_command)

    def run(self) -> int:
        if self.local_only:
            return self._run_locally()

        # Locally preprocess if needed
        if self.local_preprocess_command:
            local_cpp_cmd = _command_to_str(self.local_preprocess_command)
            if self.dry_run:
                msg(f"[dry-run only] {local_cpp_cmd}")
            cpp_status = subprocess.call(self.local_preprocess_command)
            if cpp_status != 0:
                print(
                    f"*** Local C-preprocessing failed (exit={cpp_status}): {local_cpp_cmd}"
                )
                return cpp_status

        remote_command_str = _command_to_str(self._remote_action.command)
        if self.verbose and not self.dry_run:
            msg(remote_command_str)
        if self.dry_run:
            msg(f"[dry-run only] {remote_command_str}")
            return 0

        # Remote compile C++
        try:
            return self._remote_action.run()
        # TODO: normalize absolute paths in remotely generated depfile (gcc)
        finally:
            if not self._main_args.save_temps:
                for f in self._cleanup_files:
                    os.remove(f)


def main(argv: Sequence[str]) -> int:
    cxx_remote_action = CxxRemoteAction(argv)
    return cxx_remote_action.run()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
