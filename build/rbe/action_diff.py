#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Recursively report differences between actions and their inputs.

Usage:
  $0 reproxy-logdir-1 reproxy-logdir-2 output-file-name

The first two arguments need to be directories containing .rrpl (or .rpl) files
because the scanning process writes "reproxy_log.pb" in each directory before
comparing them.
The 'output-file-name' should name an output file that appears in
both reproxy logs (and probably corresponds to actions that have
different digests).
"""

import argparse
import dataclasses
import itertools
import multiprocessing
import os
import sys

import remote_action
import reproxy_logs
import remotetool
import cl_utils
import fuchsia

from api.proxy import log_pb2
from pathlib import Path
from typing import AbstractSet, Callable, Dict, Iterable, Optional, Sequence, Tuple

_SCRIPT_BASENAME = Path(__file__).name

# TODO: move this to fuchsia.py?
_REMOTETOOL_SCRIPT = remote_action._REMOTETOOL_SCRIPT


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Recursively finds input differences of a remote output.",
    )
    parser.add_argument(
        "--reproxy_logdirs",
        type=Path,
        help="reproxy log directories, containing .rrpl or .rpl logs",
        required=True,
        metavar="DIR",
        nargs=2,
    )
    parser.add_argument(
        "--output_file",
        type=Path,
        help="remote output to compare",
        metavar="FILE",
        required=True,
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=0,
        help="Limit number of root causes to display (0=unlimited).",
        metavar="LIMIT",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


@dataclasses.dataclass
class RootCause(object):
    path: Path
    explanation: Sequence[str]

    def __str__(self) -> str:
        return "\n  ".join([f"path: {self.path}"] + self.explanation)


def verbose_root_cause(path: Path, explanation: Sequence[str], indent: str):
    for line in explanation:
        print(f"{indent}{line}")
    return RootCause(path=path, explanation=explanation)


class ActionDiffer(object):

    def __init__(
            self, left: reproxy_logs.ReproxyLog, right: reproxy_logs.ReproxyLog,
            reproxy_cfg: Dict[str, str]):
        self._left = left
        self._right = right
        self._reproxy_cfg = reproxy_cfg

    @property
    def left(self) -> reproxy_logs.ReproxyLog:
        return self._left

    @property
    def right(self) -> reproxy_logs.ReproxyLog:
        return self._right

    @property
    def reproxy_cfg(self) -> Dict[str, str]:
        return self._reproxy_cfg

    def trace(self, filepath: Path) -> Iterable[RootCause]:
        """Recursively finds action and output differences.

        Prints findings to stdout.

        Args:
          filepath: output file to compare, relative to the working dir.
              There should be a unique LogRecord of a remote action in both the
              left and right records that names this file as an output.

        Yields:
          reasons for differences.
        """
        visited = set()  # cache already visited paths
        yield from self._trace(filepath, visited, 0)

    def _trace(self, filepath: Path, visited: AbstractSet[Path],
               level: int) -> Iterable[RootCause]:
        """Recursively finds action and output differences.

        Prints findings to stdout.

        Args:
          filepath: output file to compare, relative to the working dir.
              There should be a unique LogRecord of a remote action in both the
              left and right records that names this file as an output.
          visited: set of outputs already queried (modify-by-reference).
          level: recursion level, used for formatting output.
        """
        indent = '  ' * level
        print(f"{indent}-------- Examining {filepath} --------")

        # Avoid re-visiting the same intermediate.
        if filepath in visited:
            print(f"{indent}(already visited earlier)")
            return
        else:
            visited.add(filepath)

        def root_cause(explanation: Sequence[str]) -> RootCause:
            return verbose_root_cause(filepath, explanation, indent)

        left_record = self.left.records_by_output_file.get(filepath, None)
        right_record = self.right.records_by_output_file.get(filepath, None)
        if left_record is None:
            yield root_cause(
                [f"File not found among left log's action outputs: {filepath}"])
            return
        if right_record is None:
            yield root_cause(
                [
                    f"File not found among right log's action outputs: {filepath}"
                ])
            return
        # TODO: fetch and report differences between inputs

        left_entry = left_record.remote_metadata.output_file_digests
        left_file_digest = left_entry[str(filepath)]
        right_entry = right_record.remote_metadata.output_file_digests
        right_file_digest = right_entry[str(filepath)]
        if left_file_digest == right_file_digest:
            print(
                f"{indent}Digest of {filepath} already matches: {left_file_digest}"
            )
            return

        # Digests do not match, so query the two actions that produced them
        left_action_digest = left_record.remote_metadata.action_digest
        right_action_digest = right_record.remote_metadata.action_digest
        # Possible reasons for not finding an action:
        #   * file is a source input
        #   * file was produced by a local action (possibly from racing)
        if not left_action_digest:
            yield root_cause(
                [
                    f"Could not find an action from the left log that produced {filepath}"
                ])
            return

        print(f"{indent}left action digest of {filepath}: {left_action_digest}")

        if not right_action_digest:
            yield root_cause(
                [
                    f"Could not find an action from the right log that produced {filepath}"
                ])
            return

        print(
            f"{indent}right action digest of {filepath}: {right_action_digest}")

        # Run remotetool on each action digest.
        left_action = remotetool.show_action(
            left_action_digest, self.reproxy_cfg)
        right_action = remotetool.show_action(
            right_action_digest, self.reproxy_cfg)
        diff = left_action.diff(right_action)

        have_root_cause = False
        if diff.command_unified_diffs:
            # If the commands are different, we've found one root cause
            # difference, and there is not need to analyze any further.
            yield root_cause(
                [
                    f"Actions {left_action_digest} and {right_action_digest} have different remote commands:"
                ] + diff.command_unified_diffs)
            have_root_cause = True

        platform_diffs = list(diff.platform_diffs.report())
        if platform_diffs:
            yield root_cause(
                [f"Found differences in remote action platform:"] +
                platform_diffs)
            have_root_cause = True

        if diff.input_diffs.left_only or diff.input_diffs.right_only:
            yield root_cause(
                [f"{indent}Input sets are not identical:"] +
                list(diff.input_diffs.report()))
            have_root_cause = True

        if have_root_cause:
            # Already root-caused a difference.  Stop.
            return

        # Recursively query common inputs that are different.
        # Note: iteration ordering of dictionary can be nondeterministic.
        for path, (left_digest,
                   right_digest) in diff.input_diffs.value_diffs.items():
            # Determine whether or not path refers to an input or
            # some (possibly remotely produced) intermediate.
            remote_working_dir = Path(
                left_record.command.remote_working_directory)
            if not remote_working_dir in path.parents:
                yield root_cause(
                    [
                        f"Input {path} does not come from a remote action.",
                        f"Digests: {left_digest}",
                        f"     vs. {right_digest}",
                    ])
                continue  # This is a root-cause.

            # This is an intermediate output.
            # path from show_action is relative to exec_root,
            # but trace() expects a path relative to working_dir.
            output_relpath = path.relative_to(remote_working_dir)
            print(f"{indent}Remote output file {output_relpath} differs.")
            print(f"{indent}Digests: {left_digest}")
            print(f"{indent}     vs. {right_digest}")
            print(f"{indent}[{level}](")
            yield from self._trace(output_relpath, visited, level + 1)
            print(f"{indent})[{level}]")


# Defined at the module level for multiprocessing to be able to serialize.
def _process_log(d: Path) -> reproxy_logs.ReproxyLog:
    return reproxy_logs.ReproxyLog(
        reproxy_logs.convert_reproxy_actions_log(
            reproxy_logdir=d,
            reclient_bindir=fuchsia.RECLIENT_BINDIR,
            verbose=True,
        ))


def main(argv: Sequence[str]) -> int:
    main_args = _MAIN_ARG_PARSER.parse_args(argv)

    with multiprocessing.Pool() as pool:
        logs = pool.map(_process_log, main_args.reproxy_logdirs)
    # len(logs) == 2, enforced by _MAIN_ARG_PARSER.

    with open(remotetool._REPROXY_CFG) as cfg:
        reproxy_cfg = remotetool.read_config_file_lines(cfg.readlines())

    differ = ActionDiffer(*logs, reproxy_cfg=reproxy_cfg)
    root_causes_iter = differ.trace(main_args.output_file)
    if main_args.limit == 0:
        root_causes = list(root_causes_iter)
    else:
        root_causes = list(
            itertools.islice(root_causes_iter, 0, main_args.limit))

    print("======== Summary of root causes of differences ========")
    for i, c in enumerate(root_causes):
        print(f'{i}: {c}\n')
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
