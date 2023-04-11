#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Check a command and its outputs for leaks of the output directory.

This script is both a library and standalone binary.

Usage:
  $0 [options...] [outputs...] -- command...
"""

import argparse
import io
import mmap
import os
import pathlib
import re
import subprocess
import sys

import fuchsia
import cl_utils

from pathlib import Path
from typing import Iterable, Sequence

_SCRIPT_BASENAME = Path(__file__).name

PROJECT_ROOT = fuchsia.project_root_dir()


def _relpath(path: Path, start: Path) -> Path:
    # Path.relative_to() requires self to be a subpath of the argument,
    # but here, the argument is often the subpath of self.
    # Hence, we need os.path.relpath() in the general case.
    return Path(os.path.relpath(path, start=start))


# This is a known path where remote execution occurs.
# This should only be used for workarounds as a last resort.
_REMOTE_PROJECT_ROOT = Path('/b/f/w')


def error_msg(text: str, label: str = None):
    label_text = f'[{label}]' if label else ''
    print(f'[{_SCRIPT_BASENAME}]{label_text}: Error: {text}')


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        "Scan a command for output-dir leaks.",
        argument_default=[],
    )
    parser.add_argument(
        "--label",
        type=str,
        default=None,
        help="Build system identifier for a particular action.",
    )
    parser.add_argument(
        "--execute",
        default=True,
        action="store_true",
        help="Execute the command and scan its outputs.",
    )
    parser.add_argument(
        "--no-execute",
        dest="execute",
        action="store_false",
        help="Run only pre-flight checks, and do not execute.",
    )
    # Positional args are the outputs of the command to scan.
    parser.add_argument(
        "outputs", nargs="*", help="Outputs to scan for leaks after execution.")
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def _whole_word_pattern(pattern: str) -> str:
    boundary = r'\b'
    left = '' if pattern.startswith(boundary) else boundary
    right = '' if pattern.endswith(boundary) else boundary
    return left + pattern + right


# define for easy mocking
def _open_read_text(f) -> io.TextIOBase:
    return open(f, 'rt')


def _open_read_binary(f) -> io.RawIOBase:
    return open(f, 'rb', 0)


def file_contains_subpath(
    f: Path,
    subpath: str,
    subpath_re: re.Pattern = None,
) -> bool:
    """Detect if a subpath string appears in a file.

    Args:
      f: file to scan
      subpath: path as a string to match
      subpath_re: if given, this is should be `re.compile(subpath)`.
          Passing this in avoids repeatedly compiling the same regex.

    Returns:
      True if the file's contents contains the subpath.
    """
    subpath_re = subpath_re or re.compile(
        _whole_word_pattern(subpath))  # whole word match

    if not f.exists():
        return False

    # Ignore directory outputs for now.
    if f.is_dir():
        return False

    # Don't know whether file is binary or text.
    try:  # Try text first.
        with _open_read_text(f) as lines:
            for line in lines:  # read one line at a time
                if subpath_re.search(line):
                    return True  # stop at first match
    except UnicodeDecodeError:
        # Open as binary.
        # In case file is large, use mmap() to avoid loading the entire
        # contents into memory.
        with _open_read_binary(f) as binary_file:
            s = mmap.mmap(binary_file.fileno(), 0, access=mmap.ACCESS_READ)
            # Note: This matches partial words, which risks flagging
            # false positives.
            if s.find(subpath.encode()) != -1:
                return True

    return False


def paths_with_build_dir_leaks(paths: Iterable[Path],
                               pattern: re.Pattern) -> Iterable[Path]:
    for path in paths:
        if pattern.search(str(path)):
            yield path


def tokens_with_build_dir_leaks(command: Iterable[str],
                                pattern: re.Pattern) -> Iterable[str]:
    # TODO: lex --KEY=VALUE tokens into parts and match against the parts
    for token in command:
        if pattern.search(token):
            yield token


def preflight_checks(
    paths: Iterable[Path],
    command: Iterable[str],
    pattern: re.Pattern,
    label: str = None,
) -> int:
    """Checks output paths and command for build dir leaks."""
    exit_code = 0
    output_path_leaks = list(paths_with_build_dir_leaks(paths, pattern))
    if output_path_leaks:
        for f in output_path_leaks:
            error_msg(
                f"""Output path '{f}' contains '{pattern.pattern}'.
Adding rebase_path(..., root_build_dir) in GN may fix this to be relative.
If this command requires an absolute path, mark this action in GN with
'no_output_dir_leaks = false'.""",
                label=label)
            exit_code = 1

    token_path_leaks = list(tokens_with_build_dir_leaks(command, pattern))
    for tok in token_path_leaks:
        error_msg(
            f"""Command token '{tok}' contains '{pattern.pattern}'.
Adding rebase_path(..., root_build_dir) in GN may fix this to be relative.
If this command requires an absolute path, mark this action in GN with
'no_output_dir_leaks = false'.""",
            label=label)
        exit_code = 1

    return exit_code


def postflight_checks(
    outputs: Iterable[Path],
    subpath: str,
    subpath_re: re.Pattern = None,
    label: str = None,
):
    subpath_re = subpath_re or re.compile(_whole_word_pattern(subpath))
    exit_code = 0
    # Command succeeded, scan its declared outputs.
    for f in outputs:
        if file_contains_subpath(f, subpath, subpath_re):
            error_msg(
                f"""Output file {f} contains '{subpath}'.
If this cannot be fixed in the tool, mark this action in GN with
'no_output_dir_leaks = false'.""",
                label=label)
            exit_code = 1

    return exit_code


def scan_leaks(argv: Sequence[str], exec_root: Path, working_dir: Path) -> int:
    """Scan a command and its parameters for leaks of the build dir.

    Leaks of the build-dir in commands and their output artifacts
    are harmful to action caching.

    TODO: accept additional patterns to scan, e.g. 'set_by_reclient/...'

    Args:
      argv: script args, '--', then command
      exec_root: The path to the project root, inside which all build
        command are invoked.
      working_dir: The working dir where a command is to be run.

    Returns:
      0 for success, non-zero if any errors (including the command) occurred.
    """
    try:
        ddash = argv.index('--')
    except ValueError:
        error_msg("Required '--' is missing.")
        _MAIN_ARG_PARSER.parse_args(['--help'])
        return 1

    script_args = argv[:ddash]
    command = argv[ddash + 1:]

    build_subdir = _relpath(working_dir, start=exec_root)
    build_subdir_re = re.compile(
        _whole_word_pattern(str(build_subdir)))  # match whole-word only

    main_args = _MAIN_ARG_PARSER.parse_args(script_args)
    label = main_args.label

    pre_scan_exit_code = preflight_checks(
        paths=main_args.outputs,
        command=command,
        pattern=build_subdir_re,
        label=label)

    if not main_args.execute:
        return pre_scan_exit_code

    # Invoke the original command.
    command_exit_code = subprocess.call(
        cl_utils.auto_env_prefix_command(command), cwd=working_dir)
    if command_exit_code != 0:
        return command_exit_code

    # Command succeeded, scan its declared outputs.
    post_scan_exit_code = postflight_checks(
        main_args.outputs, build_subdir, build_subdir_re, label=label)

    # return code reflects success of command and success of scans
    scan_exit_code = pre_scan_exit_code or post_scan_exit_code
    if scan_exit_code != 0:
        error_msg(
            "(See http://go/remotely-cacheable for more information.)",
            label=label)

    return scan_exit_code


def main(argv: Sequence[str]) -> int:
    return scan_leaks(
        argv,
        exec_root=PROJECT_ROOT,
        working_dir=Path(os.curdir).absolute(),
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
