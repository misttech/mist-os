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

# Local subprocess and remote environment calls need this when a
# command is prefixed with an X=Y environment variable.
_ENV = '/usr/bin/env'

# This is a known path where remote execution occurs.
# This should only be used for workarounds as a last resort.
_REMOTE_PROJECT_ROOT = '/b/f/w'

# Wrapper script to capture remote stdout/stderr.
_REMOTE_LOG_SCRIPT = 'build/rbe/log-it.sh'  # co-located with this script


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def auto_env_prefix_command(command: Sequence[str]) -> Sequence[str]:
    if not command:
        return []
    if '=' in command[0]:
        # Commands that start with X=Y local environment variables
        # need to be run with 'env'.
        return [_ENV] + command
    return command


def resolved_shlibs_from_ldd(lines: Iterable[str]) -> Iterable[str]:
    """Parse 'ldd' output.

    Args:
      lines: stdout text of 'ldd'

    Example line:
      librustc_driver-897e90da9cc472c4.so => /home/my_project/tools/rust/bin/../lib/librustc_driver.so (0x00007f6fdf600000)

    Should yield:
      /home/my_project/tools/rust/bin/../lib/librustc_driver.so
    """
    for line in lines:
        lib, sep, resolved = line.strip().partition('=>')
        if sep == '=>':
            yield resolved.strip().split(' ')[0]


def host_tool_shlibs(executable: str) -> Iterable[str]:
    """Identify shared libraries of an executable.

    This only works on platforms with `ldd`.

    Yields:
      paths to non-system shared libraries
    """
    # TODO: do this once in the entire build, as early as GN time
    # TODO: support Mac OS using `otool -L`
    ldd_output = subprocess.run(
        ['ldd', executable], capture_output=True, text=True)
    if ldd_output.returncode != 0:
        raise Exception(
            f"Failed to determine shared libraries of '{executable}'.")

    yield from resolved_shlibs_from_ldd(ldd_output.stdout.splitlines())


def host_tool_nonsystem_shlibs(executable: str) -> Iterable[str]:
    """Identify non-system shared libraries of a host tool.

    The host tool's shared libraries will need to be uploaded
    for remote execution.  (The caller should verify that
    the shared library paths fall under the remote action's exec_root.)

    Limitation: this works for only linux-x64 ELF binaries, but this is
    fine because only linux-x64 remote workers are available.

    Yields:
      paths to non-system shared libraries
    """
    for lib in host_tool_shlibs(executable):
        if any(lib.startswith(prefix) for prefix in ('/usr/lib', '/lib')):
            continue  # filter out system libs
        yield lib


def relativize_to_exec_root(path: str, start=None) -> str:
    return os.path.relpath(path, start=start or PROJECT_ROOT)


def _reclient_canonical_working_dir_components(
        subdir_components: Iterable[str]) -> Iterable[str]:
    """Computes the path used by rewrapper --canonicalize_working_dir=true.

    The exact values returned are an implementation detail of reclient
    that is not reliable, so this should only be used as a last resort
    in workarounds.

    https://team.git.corp.google.com/foundry-x/re-client/+/refs/heads/master/internal/pkg/reproxy/action.go#177

    Args:
      subdir_components: a relative path like ('out', 'default', ...)

    Yields:
      Replacement path components like ('set_by_reclient', 'a', ...)
    """
    first = next(subdir_components, None)
    if first is None or first == '':
        return  # no components
    yield 'set_by_reclient'
    for _ in subdir_components:
        yield 'a'


def reclient_canonical_working_dir(build_subdir: str) -> str:
    new_components = list(
        _reclient_canonical_working_dir_components(
            iter(build_subdir.split(os.sep))))
    return os.path.join(*new_components) if new_components else ''


def remove_working_dir_abspaths(line: str, build_subdir: str) -> str:
    # Two substutions are necesssary to accommodate both cases
    # of rewrapper --canonicalize_working_dir={true,false}.
    # TODO: if the caller knows whether which case applies, then
    # you only need to apply one of the following substitutions.
    local_working_dir_abs = os.path.join(_REMOTE_PROJECT_ROOT, build_subdir)
    canonical_working_dir = os.path.join(
        _REMOTE_PROJECT_ROOT, reclient_canonical_working_dir(build_subdir))
    return line.replace(local_working_dir_abs + os.path.sep,
                        '').replace(canonical_working_dir + os.path.sep, '')


def remove_working_dir_abspaths_from_depfile_in_place(
        depfile: str, build_subdir: str):
    # TODO(http://fxbug.dev/124714): This transformation would be more robust
    # if we properly lexed a depfile and operated on tokens instead of lines.
    with open(depfile) as f:
        new_lines = [
            remove_working_dir_abspaths(line, build_subdir) for line in f
        ]

    with open(depfile, 'w') as f:  # overwrite
        for line in new_lines:
            f.write(line)


class RemoteAction(object):
    """RemoteAction represents a command that is to be executed remotely."""

    def __init__(
        self,
        rewrapper: str,
        command: Sequence[str],
        options: Sequence[str] = None,
        exec_root: Optional[str] = None,
        working_dir: str = None,
        inputs: Sequence[str] = None,
        output_files: Sequence[str] = None,
        output_dirs: Sequence[str] = None,
        save_temps: bool = False,
        auto_reproxy: bool = False,
        remote_log: str = "",
        fsatrace_path: str = "",
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
          remote_log: "" means disabled.  Any other value, remote logging is
            enabled, and stdout/stderr of the remote execution is captured
            to a file and downloaded.
            if "<AUTO>":
              if there is at least one remote output file:
                name the log "${output_files[0]}.remote-log"
              else:
                name the log "rbe-action-output.remote-log"
            else:
              use the given name appended with ".remote-log"
          fsatrace_path: Given a path to an fsatrace tool
              (located under exec_root), this will wrap the remote command
              to trace and log remote file access.
              if there is at least one remote output file:
                the trace name is "${output_files[0]}.remote-fsatrace"
              else:
                the trace name "rbe-action-output.remote-fsatrace"
        """
        self._rewrapper = rewrapper
        self._save_temps = save_temps
        self._auto_reproxy = auto_reproxy
        self._working_dir = os.path.abspath(working_dir or os.curdir)
        self._exec_root = os.path.abspath(exec_root or PROJECT_ROOT)
        # Parse and strip out --remote-* flags from command.
        remote_args, self._remote_command = REMOTE_FLAG_ARG_PARSER.parse_known_args(
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

        # Amend input/outputs when logging remotely.
        self._remote_log_name = self._name_remote_log(remote_log)
        if self._remote_log_name:
            # These paths are relative to the working dir.
            self._output_files.append(self._remote_log_name)
            self._inputs.append(self._remote_log_script_path)

        self._fsatrace_path = fsatrace_path  # relative to working dir
        if self._fsatrace_path:
            self._inputs.extend([self._fsatrace_path, self._fsatrace_so])
            self._output_files.append(self._fsatrace_remote_log)

        self._cleanup_files = []

    @property
    def exec_root(self) -> str:
        return self._exec_root

    @property
    def _default_auxiliary_file_basename(self) -> str:
        if self._output_files:
            return self._output_files[0]
        else:  # pick something arbitrary, but deterministic
            return 'rbe-action-output'

    def _name_remote_log(self, remote_log) -> str:
        if remote_log == '<AUTO>':
            return self._default_auxiliary_file_basename + '.remote-log'

        if remote_log:
            return remote_log + '.remote-log'

        return None

    @property
    def _remote_log_script_path(self) -> str:
        return os.path.join(self.exec_root_rel, _REMOTE_LOG_SCRIPT)

    @property
    def _fsatrace_local_log(self) -> str:
        return self._default_auxiliary_file_basename + '.local-fsatrace'

    @property
    def _fsatrace_remote_log(self) -> str:
        return self._default_auxiliary_file_basename + '.remote-fsatrace'

    @property
    def _fsatrace_so(self) -> str:
        # fsatrace needs the corresponding .so to work
        return self._fsatrace_path + '.so'

    @property
    def local_command(self) -> Sequence[str]:
        """This is the original command that would have been run locally.
        All of the --remote-* flags have been removed at this point.
        """
        return auto_env_prefix_command(self._remote_command)

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
    def working_dir(self) -> str:
        return self._working_dir

    @property
    def remote_disable(self) -> bool:
        return self._remote_disable

    def _relativize_path_to_exec_root(self, path: str) -> str:
        return relativize_to_exec_root(
            os.path.normpath(os.path.join(self.working_dir, path)),
            start=self.exec_root)

    def _relativize_paths_to_exec_root(self,
                                       paths: Sequence[str]) -> Sequence[str]:
        return [self._relativize_path_to_exec_root(path) for path in paths]

    @property
    def exec_root_rel(self) -> str:
        return os.path.relpath(self.exec_root, start=self.working_dir)

    @property
    def build_subdir(self) -> str:
        """This is the relative path from the exec_root to the current working dir."""
        return os.path.relpath(self.working_dir, start=self.exec_root)

    @property
    def inputs_relative_to_working_dir(self) -> Sequence[str]:
        return self._inputs

    @property
    def output_files_relative_to_working_dir(self) -> Sequence[str]:
        return self._output_files

    @property
    def output_dirs_relative_to_working_dir(self) -> Sequence[str]:
        return self._output_dirs

    @property
    def inputs_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_paths_to_exec_root(
            self.inputs_relative_to_working_dir)

    @property
    def output_files_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_paths_to_exec_root(
            self.output_files_relative_to_working_dir)

    @property
    def output_dirs_relative_to_project_root(self) -> Sequence[str]:
        return self._relativize_paths_to_exec_root(
            self.output_dirs_relative_to_working_dir)

    def _inputs_list_file(self) -> str:
        inputs_list_file = self._output_files[0] + '.inputs'
        contents = '\n'.join(self.inputs_relative_to_project_root) + '\n'
        with open(inputs_list_file, 'w') as f:
            f.write(contents)
        return inputs_list_file

    def _generate_rewrapper_command_prefix(self) -> Iterable[str]:
        yield self._rewrapper
        yield f"--exec_root={self.exec_root}"
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

    @property
    def _remote_log_command_prefix(self) -> Sequence[str]:
        return [
            self._remote_log_script_path,
            '--log',
            self._remote_log_name,
            '--',
        ]

    def _fsatrace_command_prefix(self, log: str) -> Sequence[str]:
        return [
            _ENV,
            'FSAT_BUF_SIZE=5000000',
            self._fsatrace_path,
            'erwdtmq',
            log,
            '--',
        ]

    def _generate_command(self) -> Iterable[str]:
        """Generates the rewrapper command, one token at a time."""
        if not self._remote_disable:
            # TODO(http://fxbug.dev/124190): detect that reproxy is needed, by checking the environment
            if self._auto_reproxy:
                yield fuchsia.REPROXY_WRAP
                yield '--'

            yield from self._generate_rewrapper_command_prefix()
            yield '--'

            if self._remote_log_name:
                yield from self._remote_log_command_prefix

            # When requesting both remote logging and fsatrace,
            # use fsatrace as the inner wrapper because the user is not
            # likely to be interested in fsatrace entries attributed
            # to the logging wrapper.
            if self._fsatrace_path:
                yield from self._fsatrace_command_prefix(
                    self._fsatrace_remote_log)
        else:
            # When requesting fsatrace, log to a different file than the
            # remote log, so they can be compared.
            if self._fsatrace_path:
                yield from self._fsatrace_command_prefix(
                    self._fsatrace_local_log)

        yield from self.local_command

    @property
    def command(self) -> Sequence[str]:
        """This is the fully constructed rewrapper command executed on the host."""
        return list(self._generate_command())

    @property
    def command_quoted_str(self) -> str:
        return ' '.join(shlex.quote(t) for t in self.command)

    # features to port over from fuchsia-rbe-action.sh:
    # TODO(http://fxbug.dev/123178): facilitate delayed downloads using --action_log

    def _cleanup(self):
        for f in self._cleanup_files:
            if os.path.exists(f):
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
            return subprocess.call(self.command, cwd=self.working_dir)
        finally:
            if not self._save_temps:
                self._cleanup()

    def run_with_main_args(self, main_args: argparse.Namespace) -> int:
        """Run depending on verbosity and dry-run mode.

        This serves as a template for main() programs whose
        primary execution action is RemoteAction.run().

        Args:
          main_args: struct with (.verbose, .dry_run, .label)

        Returns:
          exit code
        """
        command_str = self.command_quoted_str
        if main_args.verbose and not main_args.dry_run:
            msg(command_str)
        if main_args.dry_run:
            label_str = " "
            if main_args.label:
                label_str += f"[{main_args.label}] "
            msg(f"[dry-run only]{label_str}{command_str}")
            return 0

        return self.run()


def _rewrapper_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        "Understand some rewrapper flags, so they may be used as attributes.",
        argument_default=[],
    )
    parser.add_argument(
        "--exec_root",
        type=str,
        default="",
        help="Root directory from which all inputs/outputs are contained.",
    )
    return parser


_REWRAPPER_ARG_PARSER = _rewrapper_arg_parser()


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


REMOTE_FLAG_ARG_PARSER = _remote_flag_arg_parser()


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
        metavar="PATH",
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
        const="<AUTO>",  # pick name based on ${output_files[0]}
        default="",  # blank means to not log
        metavar="BASE",
        nargs='?',
        help="""Capture remote execution's stdout/stderr to a log file.
If a name argument BASE is given, the output will be 'BASE.remote-log'.
Otherwise, BASE will default to the first output file named.""",
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
    group.add_argument(
        "--fsatrace-path",
        type=str,
        default="",  # blank means do not trace
        metavar="PATH",
        help="""Given a path to an fsatrace tool (located under exec_root), this will trace a remote execution's file accesses.  This is useful for diagnosing unexpected differences between local and remote builds.  The trace file will be named '{output_files[0]}.remote-fsatrace' (if there is at least one output), otherwise 'remote-action-output.remote-fsatrace'.""",
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
        remote_log=main_args.remote_log,
        fsatrace_path=main_args.fsatrace_path,
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

    return remote_action.run_with_main_args(main_args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
