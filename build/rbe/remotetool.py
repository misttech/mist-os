#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functions for working with remotetool.

This is a library and standalone executable script.
"""

import dataclasses
import difflib
import os
import shlex
import subprocess
import sys

from pathlib import Path
from typing import Any, Dict, Iterable, Sequence, Tuple

import fuchsia
import cl_utils

_SCRIPT_BASENAME = Path(__file__).name
_SCRIPT_DIR = Path(__file__).parent
PROJECT_ROOT = fuchsia.project_root_dir()
PROJECT_ROOT_REL = cl_utils.relpath(PROJECT_ROOT, start=os.curdir)

_REPROXY_CFG = _SCRIPT_DIR / "fuchsia-reproxy.cfg"

_HOST_REMOTETOOL = fuchsia.RECLIENT_BINDIR / 'remotetool'

class ParseError(ValueError):

    def __init__(self, msg: str):
        super().__init__(msg)


def _must_partition_string(text: str, sep: str) -> Tuple[str, str]:
    before, found_sep, after = text.partition(sep)
    if found_sep != sep:
        raise ParseError(
            f"Expected but failed to find \"{sep}\" in text \"{text}\".")
    return before, after


def _must_partition_sequence(seq: Sequence[str],
                             sep: str) -> Tuple[Sequence[str], Sequence[str]]:
    before, found_sep, after = cl_utils.partition_sequence(seq, sep)
    if found_sep != sep:
        raise ParseError(f"Expected but failed to find line == \"{sep}\".")
    return before, after


def _parse_input_digest(line: str) -> Tuple[Path, str]:
    path, right = _must_partition_string(line, ':')
    ignored, right = _must_partition_string(right, ':')
    digest = right.lstrip().rstrip(']')
    return Path(path), digest


def _parse_output_digest(line: str) -> Tuple[Path, str]:
    path, right = _must_partition_string(line, ',')
    ignored, right = _must_partition_string(right, ':')
    digest = right.lstrip()
    return Path(path), digest


# TODO: move this to a library for abstract data operations
class DictionaryDiff(object):
    """Representation of a difference between dictionaries."""

    def __init__(self, left: Dict[Any, Any], right: Dict[Any, Any]):
        left_keys = set(left.keys())
        right_keys = set(right.keys())
        self._left_only = {k: left[k] for k in left_keys - right_keys}
        self._right_only = {k: right[k] for k in right_keys - left_keys}
        common_keys = left_keys.intersection(right_keys)
        paired = [(k, (left[k], right[k])) for k in common_keys]
        self._value_diffs = {k: (v[0], v[1]) for k, v in paired if v[0] != v[1]}
        self._matches = {k: v[0] for k, v in paired if v[0] == v[1]}

    @property
    def left_only(self) -> Dict[Any, Any]:
        return self._left_only

    @property
    def right_only(self) -> Dict[Any, Any]:
        return self._right_only

    @property
    def value_diffs(self) -> Dict[Any, Tuple[Any, Any]]:
        return self._value_diffs

    @property
    def matches(self) -> Dict[Any, Any]:
        return self._matches

    def report(self) -> Iterable[str]:
        yield from (f"left only: {k}: {v}" for k, v in self.left_only.items())
        yield from (f"right only: {k}: {v}" for k, v in self.right_only.items())
        yield from (
            f"different values: {k}: {v1} vs. {v2}"
            for k, (v1, v2) in self.value_diffs.items())


@dataclasses.dataclass
class ActionDifferences(object):
    command_unified_diffs: Sequence[str]
    input_diffs: DictionaryDiff
    platform_diffs: DictionaryDiff


@dataclasses.dataclass
class ShowActionResult(object):
    """Structured representation of `remotetool --operation show_action`."""

    command: Sequence[str]
    platform: Dict[str, str]
    inputs: Dict[Path, str]
    output_files: Dict[Path, str]

    def __eq__(self, other) -> bool:
        return self.command == other.command and self.platform == other.platform and self.inputs == other.inputs and self.output_files == other.output_files

    def diff(self, other: 'ShowActionResult') -> ActionDifferences:
        return ActionDifferences(
            command_unified_diffs=list(
                difflib.unified_diff(
                    self.command,
                    other.command,
                    fromfile="left action",
                    tofile="right action",
                )),
            input_diffs=DictionaryDiff(self.inputs, other.inputs),
            platform_diffs=DictionaryDiff(self.platform, other.platform),
        )


def parse_show_action_output(lines: Iterable[str]) -> ShowActionResult:
    """Parse the results of `remotetool --operation show_action`.

    Returns:
      ShowActionResult, containing command, inputs, outputs.

    Raises:
      ParseError if there are any parsing errors.
    """
    stripped_lines = [line.rstrip() for line in lines]
    preamble, remainder = _must_partition_sequence(stripped_lines, 'Command')
    command_section, remainder = _must_partition_sequence(remainder, 'Platform')
    platform_section, remainder = _must_partition_sequence(remainder, 'Inputs')
    inputs_section, remainder = _must_partition_sequence(
        remainder, 'Action Result')
    result_section, remainder = _must_partition_sequence(
        remainder, 'Output Files')
    output_files_section, remainder = _must_partition_sequence(
        remainder, 'Output Files From Directories')
    # command_section has:
    # [0] '======='
    # [1] 'Command Digest: ...'
    # [2] <the command in one long line>
    # [3] <blank line>
    command = shlex.split(command_section[2].lstrip())

    # platform_section has:
    # [0] '======='
    # [1:] key=value (repeated)
    platform_parameters = {
        k: v for k, _, v in (
            line.lstrip().partition('=') for line in platform_section[1:-1])
    }

    # inputs_section has:
    # [0] '======='
    # [1] '[Root directory digest: ...]
    # [2:] <path>: [File digest: <digest>]
    # [-2] <blank line>
    # [-1] '------------------------------------------------------------------------'
    # Input paths are relative to the exec_root.
    # Inputs that come from other remote actions, might start with
    # 'set_by_reclient/a' (relative to exec_root, canonicalized).
    inputs = {
        k: v for k, v in (
            _parse_input_digest(line) for line in inputs_section[2:-2])
    }

    # result_section:
    # (not used)

    # output_files_section has:
    # [0] '======='
    # [1:] <path>: [File digest: <digest>]
    # [-1] <blank line>
    # Output file paths are relative to the working_dir (not exec_root).
    output_files = {
        k: v for k, v in (
            _parse_output_digest(line) for line in output_files_section[1:-1])
    }

    return ShowActionResult(
        command=command,
        inputs=inputs,
        output_files=output_files,
        platform=platform_parameters,
    )


def read_config_file_lines(lines: Iterable[str]) -> Dict[str, str]:
    """Generic parser for reading flag files.

    Args:
      lines: lines of config from a file

    Returns:
      dictionary of key-value pairs read from the config file.
    """
    result = {}
    for line in lines:
        stripped = line.strip()
        if stripped and not stripped.startswith('#'):
            key, sep, value = stripped.partition('=')
            if sep == '=':
                result[key] = value
    return result


class RemoteTool(object):

    def __init__(self, reproxy_cfg: Path):
        self._reproxy_cfg = reproxy_cfg

    def __eq__(self, other) -> bool:
        return self.config == other.config

    @property
    def config(self) -> Dict[str, str]:
        return self._reproxy_cfg

    def run(
        self,
        args: Sequence[str],
        show_command: bool = False,
        **kwargs,
    ) -> cl_utils.SubprocessResult:
        """Runs `remotetool` using the given reproxy configuration.

        Args:
          args: remotetool command-line arguments
          show_command: if True, print the full remotetool command before executing.
          **kwargs: additional arguments forwarded to cl_utils.subprocess_call.

        Returns:
          SubprocessResult with exit code and captured stdout.
        """
        service = self.config["service"]
        instance = self.config["instance"]
        auto_args = [
            f'--service={service}',
            f'--instance={instance}',
        ]

        # Infra builds use GCE credentials instead of ADC.
        gce_creds = os.environ.get("RBE_use_gce_credentials", None)
        if gce_creds:
            auto_args.append(f'--use_gce_credentials={gce_creds}')
        else:
            use_adc = self.config.get("use_application_default_credentials", None)
            if use_adc:
                auto_args.append(f"--use_application_default_credentials={use_adc}")

        command = [
            str(PROJECT_ROOT_REL / _HOST_REMOTETOOL),
        ] + auto_args + args
        command_str = cl_utils.command_quoted_str(command)
        if show_command:
            print(command_str)

        result = cl_utils.subprocess_call(command, **kwargs)
        if result.returncode != 0:
            for line in result.stderr:
                print(line)
            raise subprocess.CalledProcessError(result.returncode, command)
        return result

    def show_action(self, digest: str, **kwargs) -> ShowActionResult:
        """Reads parameters of a remote action using `remotetool`.

        Args:
          digest: the hash/size of the action to lookup.
          **kwargs: arguments forwarded to cl_utils.subprocess_call.

        Returns:
          ShowActionResult describing command, inputs, outputs.
        """
        args = ['--operation', 'show_action', '--digest', digest]
        final_kwargs = kwargs
        final_kwargs["quiet"] = True
        result = self.run(args, **final_kwargs)
        return parse_show_action_output(result.stdout)

    def download_blob(
            self, path: Path, digest: str,
            **kwargs) -> cl_utils.SubprocessResult:
        """Downloads a remote artifact.

        Args:
          path: location of output to download.
          digest: the hash/size of the blob to retrieve.
          **kwargs: arguments forwarded to cl_utils.subprocess_call.

        Returns:
          return code of remotetool
        """
        # TODO: use filelock.FileLock to guard against concurrent conflicts
        args = [
            '--operation', 'download_blob', '--digest', digest, '--path',
            str(path)
        ]
        final_kwargs = kwargs
        final_kwargs["quiet"] = True
        return self.run(args, **final_kwargs)

    def download_dir(
            self, path: Path, digest: str,
            **kwargs) -> cl_utils.SubprocessResult:
        """Downloads a remote directory of artifacts.

        Args:
          path: location of output to download.
          digest: the hash/size of the dir to retrieve.
          **kwargs: arguments forwarded to cl_utils.subprocess_call.

        Returns:
          return code of remotetool
        """
        # TODO: use filelock.FileLock to guard against concurrent conflicts
        args = [
            '--operation', 'download_dir', '--digest', digest, '--path',
            str(path)
        ]
        final_kwargs = kwargs
        final_kwargs["quiet"] = True
        return self.run(args, **final_kwargs)


def main(argv: Sequence[str]) -> int:
    with open(_REPROXY_CFG) as cfg:
        tool = RemoteTool(read_config_file_lines(cfg))
    result = tool.run(argv, show_command=True, quiet=False)
    return result.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
