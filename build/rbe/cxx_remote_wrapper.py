#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Remotely compile C and C++.

This script functions as a standalone executable.

Usage:
  $0 [remote options...] -- compile-comand...
"""

import argparse
import os
import subprocess
import sys

import cxx
import cl_utils
import depfile
import fuchsia
import remote_action

from pathlib import Path
from typing import Any, Iterable, Optional, Sequence

_SCRIPT_BASENAME = Path(__file__).name
_SCRIPT_DIR = Path(__file__).parent


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


REMOTE_COMPILER_SWAPPER = _SCRIPT_DIR / "cxx-swap-remote-compiler.sh"


def _main_arg_parser() -> argparse.ArgumentParser:
    """Construct the argument parser, called by main()."""
    parser = argparse.ArgumentParser(
        description="Prepares a C++ command for remote execution.",
        argument_default=[],
        add_help=True,  # Want this to exit after printing --help
    )
    remote_action.inherit_main_arg_parser_flags(parser)
    group = parser.add_argument_group(
        title="C++ remote wrapper",
        description="C++ remote action options",
    )
    group.add_argument(
        "--cpp-strategy",
        type=str,
        choices=['auto', 'local', 'integrated'],
        default='auto',
        metavar='STRATEGY',
        help="""Configure how C-preprocessing is done.
    integrated: preprocess and compile in a single step,
    local: preprocess locally, compile remotely,
    auto (default): one of the above, chosen by the script automatically.
""",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def check_missing_remote_tools(
        compiler_type: cxx.Compiler,
        project_root: Path = None) -> Iterable[Path]:
    """Check for the existence of tools needed for remote execution.

    Args:
      compiled_type (cxx.Compiler): clang or gcc
      project_root: location of project root from which the compilers
        can be found.  Path may be absolute or relative.

    Yields:
      Paths to missing but required tool directories.
    """
    root = project_root or remote_action.PROJECT_ROOT_REL
    required_remote_tools = []
    if compiler_type == cxx.Compiler.CLANG:
        required_remote_tools.append(fuchsia.REMOTE_CLANG_SUBDIR)
    if compiler_type == cxx.Compiler.GCC:
        required_remote_tools.append(fuchsia.REMOTE_GCC_SUBDIR)

    for d in required_remote_tools:
        if not (root / d).is_dir():
            yield d


class CxxRemoteAction(object):

    def __init__(
            self,
            argv: Sequence[str],
            exec_root: Path = None,
            working_dir: Path = None,
            host_platform: str = None,
            auto_reproxy: bool = True,  # can disable for unit-testing
    ):
        self._working_dir = (working_dir or Path(os.curdir)).absolute()
        self._exec_root = (exec_root or remote_action.PROJECT_ROOT).absolute()
        self._host_platform = host_platform or fuchsia.HOST_PREBUILT_PLATFORM

        # Propagate --remote-flag=... options to the remote prefix,
        # as if they appeared before '--'.
        # Forwarded rewrapper options with values must be written as '--flag=value',
        # not '--flag value' because argparse doesn't know what unhandled flags
        # expect values.
        main_argv, filtered_command = remote_action.forward_remote_flags(argv)

        # forward all unknown flags to rewrapper
        # --help here will result in early exit()
        self._main_args, self._main_remote_options = _MAIN_ARG_PARSER.parse_known_args(
            main_argv)

        # Re-launch with reproxy if needed.
        if auto_reproxy:
            remote_action.auto_relaunch_with_reproxy(
                script=Path(__file__), argv=argv, args=self._main_args)

        if not filtered_command:  # there is no command, bail out early
            return
        self._cxx_action = cxx.CxxAction(command=filtered_command)

        # Determine whether this action can be done remotely.
        self._local_only = self._main_args.local or self._detect_local_only()

        self._local_preprocess_command = None
        self._cpp_strategy = self._resolve_cpp_strategy()

        self._prepare_status = None
        self._cleanup_files = []  # Sequence[Path]
        self._remote_action = None

    @property
    def compiler_path(self) -> Path:
        return self.cxx_action.compiler.tool

    @property
    def compiler_type(self) -> cxx.Compiler:
        return self.cxx_action.compiler.type

    @property
    def depfile(self) -> Optional[Path]:
        return self.cxx_action.depfile

    def _depfile_exists(self) -> bool:
        # Defined for easy precise mocking.
        return self.depfile and self.depfile.exists()

    def check_preconditions(self):
        if not self.cxx_action.target and self.cxx_action.compiler_is_clang:
            raise Exception(
                "For remote compiling with clang, an explicit --target is required, but is missing."
            )

        if self._main_args.fsatrace_path:
            msg(
                'Warning: Due to http://fxbug.dev/128947, remote fsatrace does not work with C++ mode as-is, because the fsatrace prefix confuses the re-client C++ input processor.  Automatically disabling --fsatrace-path.'
            )
            self._main_args.fsatrace_path = None

        # check for required remote tools
        missing_required_tools = list(
            check_missing_remote_tools(self.compiler_type, self.exec_root_rel))
        if missing_required_tools:
            raise Exception(
                f"Missing the following tools needed for remote compiling C++: {missing_required_tools}.  See tqr/563535 for how to fetch the needed packages."
            )

    @property
    def command_line_inputs(self) -> Sequence[Path]:
        return [
            Path(p)
            for p in cl_utils.flatten_comma_list(self._main_args.inputs)
        ]

    @property
    def command_line_output_files(self) -> Sequence[Path]:
        return [
            Path(p)
            for p in cl_utils.flatten_comma_list(self._main_args.output_files)
        ]

    @property
    def command_line_output_dirs(self) -> Sequence[Path]:
        return [
            Path(p) for p in cl_utils.flatten_comma_list(
                self._main_args.output_directories)
        ]

    def _post_remote_success_action(self) -> int:
        # Remotely generated gcc depfiles may contain absolute paths
        # that are not suitable for local use.  Rewrite them.
        if self.compiler_type == cxx.Compiler.GCC and self._depfile_exists():
            self._rewrite_remote_depfile()
        # TODO: if downloads were skipped, need to force-download depfile
        return 0

    def _rewrite_remote_depfile(self):
        remote_action.rewrite_depfile(
            dep_file=self.working_dir / self.depfile,  # in-place
            transform=self.remote_action._relativize_remote_or_local_deps,
        )

    def _remote_output_files(self) -> Sequence[Path]:
        return list(
            self.cxx_action.output_files()) + self.command_line_output_files

    def prepare(self) -> int:
        """Setup everything ahead of remote execution."""
        assert not self.local_only, "This should not be reached in local-only mode."

        if self._prepare_status is not None:
            return self._prepare_status

        self.check_preconditions()

        # evaluate the separate preprocessing and compile-preprocessed command
        # even if we won't use them.
        self._local_preprocess_command, self._compile_preprocessed_command = self.cxx_action.split_preprocessing(
        )

        remote_inputs = list(
            self.cxx_action.input_files()) + self.command_line_inputs
        if self.cpp_strategy == 'local':
            # preprocess locally, then compile the result remotely
            preprocessed_source = self.cxx_action.preprocessed_output
            self._cleanup_files.append(preprocessed_source)
            remote_inputs.append(preprocessed_source)
            cpp_status = self.preprocess_locally()
            remote_command = self._compile_preprocessed_command
            if cpp_status != 0:
                return cpp_status

        elif self.cpp_strategy == 'integrated':
            # preprocess driven by the compiler, done remotely
            remote_command = self.cxx_action.command
            # TODO: might need -Wno-constant-logical-operand to workaround
            #   ZX_DEBUG_ASSERT.

        # Prepare remote compile action
        remote_output_dirs = list(
            self.cxx_action.output_dirs()) + self.command_line_output_dirs
        remote_options = [
            "--labels=type=compile,compiler=clang,lang=cpp",  # TODO: gcc?
            "--canonicalize_working_dir=true",
        ] + self._main_remote_options  # allow forwarded options to override defaults

        # The output file is inferred automatically by rewrapper in C++ mode,
        # but naming it explicitly here makes it easier for RemoteAction
        # to use the output file name for other auxiliary files.
        remote_output_files = self._remote_output_files()

        # Workaround b/239101612: missing gcc support libexec binaries for remote build
        if self.compiler_type == cxx.Compiler.GCC:
            remote_inputs.extend(
                list(fuchsia.gcc_support_tools(self.compiler_path)))

        # Support for remote cross-compilation:
        if self.host_platform != fuchsia.REMOTE_PLATFORM:
            # compiler path is relative to current working dir
            compiler_swapper_rel = os.path.relpath(
                REMOTE_COMPILER_SWAPPER, start=self.working_dir)
            remote_inputs.extend([self.remote_compiler, compiler_swapper_rel])
            # Let --remote_wrapper apply the prefix to the command remotely.
            remote_options.append(f'--remote_wrapper={compiler_swapper_rel}')

        self.vprintlist('remote inputs', remote_inputs)
        self.vprintlist('remote output files', remote_output_files)
        self.vprintlist('remote output dirs', remote_output_dirs)
        self.vprintlist('rewrapper options', remote_options)

        downloads = []
        if self.depfile:  # always fetch the depfile
            downloads.append(self.depfile)

        self._remote_action = remote_action.remote_action_from_args(
            main_args=self._main_args,
            remote_options=remote_options,
            command=remote_command,
            inputs=remote_inputs,
            output_files=remote_output_files,
            output_dirs=remote_output_dirs,
            working_dir=self.working_dir,
            exec_root=self.exec_root,
            post_remote_run_success_action=self._post_remote_success_action,
            downloads=downloads,
        )

        self._prepare_status = 0
        return self._prepare_status

    @property
    def remote_action(self) -> remote_action.RemoteAction:
        return self._remote_action

    @property
    def working_dir(self) -> Path:
        return self._working_dir

    @property
    def exec_root(self) -> Path:
        return self._exec_root

    @property
    def exec_root_rel(self) -> Path:
        return cl_utils.relpath(self.exec_root, start=self.working_dir)

    @property
    def build_subdir(self) -> Path:  # relative
        """This is the relative path from the exec_root to the current working dir."""
        return self.working_dir.relative_to(self.exec_root)

    @property
    def host_platform(self) -> str:
        return self._host_platform

    @property
    def verbose(self) -> bool:
        return self._main_args.verbose

    @property
    def dry_run(self) -> bool:
        return self._main_args.dry_run

    def vmsg(self, text: str):
        if self.verbose:
            msg(text)

    def vprintlist(self, desc: str, items: Iterable[Any]):
        """In verbose mode, print elements.

        Args:
          desc: text description of what is being printed.
          items: stream of any type of object that is str-able.
        """
        if self.verbose:
            msg(f'{desc}: {{')
            for item in items:
                text = str(item)
                print(f'  {text}')
            print(f'}}  # {desc}')

    def _resolve_cpp_strategy(self) -> str:
        """Resolve preprocessing strategy to 'local' or 'integrated'."""
        cpp_strategy = self._main_args.cpp_strategy
        if cpp_strategy == 'auto':
            if self.cxx_action.uses_macos_sdk:
                # Mac SDK headers reside outside of exec_root,
                # which doesn't work for remote compiling.
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

    def _detect_local_only(self) -> bool:
        if self.cxx_action.sources:
            first_source = self.cxx_action.sources[0]
            if first_source.file.name.endswith('.S'):
                # Compiling un-preprocessed assembly is not supported remotely.
                return True
            elif first_source.dialect not in {cxx.SourceLanguage.C,
                                              cxx.SourceLanguage.CXX}:
                # e.g. Obj-C must be compiled locally
                return True

        return False

    @property
    def check_determinism(self) -> bool:
        return self._main_args.check_determinism

    @property
    def miscomparison_export_dir(self) -> Optional[Path]:
        if self._main_args.miscomparison_export_dir:
            return self.working_dir / self._main_args.miscomparison_export_dir
        return None

    @property
    def label(self) -> Optional[str]:
        return self._main_args.label

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
    def remote_compiler(self) -> Path:
        return fuchsia.remote_executable(self.cxx_action.compiler.tool)

    def _run_locally(self) -> int:
        export_dir = self.miscomparison_export_dir
        if self.check_determinism:
            self.vmsg(
                "Running the original compile command (with -Wdate-time) locally twice and comparing outputs."
            )
            command = fuchsia.check_determinism_command(
                exec_root=self.exec_root_rel,
                outputs=self._remote_output_files(),
                command=self.original_compile_command,
                miscomparison_export_dir=(
                    export_dir / self.build_subdir if export_dir else None),
                label=self.label,
            )
            # Both clang and gcc support -Wdate-time to catch nonreproducible
            # builds.  This can induce a failure earlier, without having
            # to build twice and compare.
            command += ['-Wdate-time']
        else:
            self.vmsg("Running the original compile command locally.")
            command = self.original_compile_command

        exit_code = subprocess.call(command, cwd=self.working_dir)

        # It would be nice if we could upload the set of inputs when
        # compilation fails the determinism check.
        # For C++, the complete set of input is computed by re-client's
        # input processor, and today, we don't have a way to directly access
        # that information (without actually requesting remote execution).
        # We don't have a remote action_digest either because determinism is a
        # local-only check.

        return exit_code

    def preprocess_locally(self) -> int:
        # Locally preprocess if needed
        local_cpp_cmd = cl_utils.command_quoted_str(
            self.local_preprocess_command)
        if self.dry_run:
            msg(f"[dry-run only] {local_cpp_cmd}")
            return 0
        self.vmsg(f"Local C-preprocessing: {local_cpp_cmd}")

        cpp_status = subprocess.call(self.local_preprocess_command)
        if cpp_status != 0:
            print(
                f"*** Local C-preprocessing failed (exit={cpp_status}): {local_cpp_cmd}"
            )
        return cpp_status

    def _run_remote_action(self) -> int:
        return self.remote_action.run_with_main_args(self._main_args)

    def run(self) -> int:
        if self.local_only:
            return self._run_locally()

        prepare_status = self.prepare()
        if prepare_status != 0:
            return prepare_status

        # Remote compile C++
        try:
            return self._run_remote_action()
        # TODO: normalize absolute paths in remotely generated depfile (gcc)
        finally:
            if not self._main_args.save_temps:
                self._cleanup()

    def _cleanup(self):
        for f in self._cleanup_files:
            f.unlink()


def main(argv: Sequence[str]) -> int:
    cxx_remote_action = CxxRemoteAction(
        argv,  # [remote options] -- C-compile-command...
        exec_root=remote_action.PROJECT_ROOT,
        working_dir=Path(os.curdir),
        host_platform=fuchsia.HOST_PREBUILT_PLATFORM,
    )
    return cxx_remote_action.run()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
