#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functions for working with reproxy logs.

This requires python protobufs for reproxy LogDump.
"""

import argparse
import hashlib
import multiprocessing
import os
import shutil
import subprocess
import sys

from api.log import log_pb2
from pathlib import Path
from typing import Callable, Dict, Iterable, Sequence, Tuple

import fuchsia

_SCRIPT_BASENAME = Path(__file__).name


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


class ReproxyLog(object):
    """Contains a LogDump proto and indexes information internally.

  The output_file paths used are relative to the working dir.
  """

    def __init__(self, log_dump: log_pb2.LogDump):
        self._proto: log_pb2.LogDump = log_dump

        # Index in a few ways for easy lookup.
        self._records_by_output_file = self._index_records_by_output_file()
        self._records_by_action_digest = self._index_records_by_action_digest()

    def _index_records_by_output_file(self) -> Dict[Path, log_pb2.LogRecord]:
        """Map files to the records of the actions that created them."""
        records: Dict[Path, log_pb2.LogRecord] = dict()
        for record in self.proto.records:
            for output_file in record.command.output.output_files:
                records[Path(output_file)] = record
        return records

    def _index_records_by_action_digest(self) -> Dict[str, log_pb2.LogRecord]:
        """Map action records by their action digest (hash/size)."""
        return {
            record.remote_metadata.action_digest: record
            for record in self.proto.records
            # Not all actions have remote_metadata, due to local/racing.
            # If there happens to be duplicates (due to retries),
            # just keep the last one.
            if record.HasField('remote_metadata') and
            record.remote_metadata.action_digest
        }

    @property
    def proto(self) -> log_pb2.LogDump:
        return self._proto

    @property
    def records_by_output_file(self) -> Dict[Path, log_pb2.LogRecord]:
        return self._records_by_output_file

    @property
    def records_by_action_digest(self) -> Dict[str, log_pb2.LogRecord]:
        return self._records_by_action_digest

    def diff_by_outputs(
        self, other: 'ReproxyLog',
        eq_comparator: Callable[[log_pb2.LogRecord, log_pb2.LogRecord], bool]
    ) -> Iterable[Tuple[Path, log_pb2.LogRecord, log_pb2.LogRecord]]:
        """Compare actions output-by-output.

        Args:
          other: the other log to compare.
          eq_comparator: compare function for LogRecord.
              When this evaluates to False, report a difference.

        Yields:
          pairs of (left, right) log records with key differences.
        """
        # Find output files common to both logs.
        # Non-common files are ignored.
        left_keys = set(self.records_by_output_file.keys())
        right_keys = set(other.records_by_output_file.keys())
        common_keys = left_keys & right_keys
        visited_outputs = set()
        for k in common_keys:
            if k in visited_outputs:
                continue
            left_record = self.records_by_output_file[k]
            right_record = other.records_by_output_file[k]
            if not eq_comparator(left_record, right_record):
                yield (k, left_record, right_record)
            left_outputs = set(
                Path(p) for p in left_record.command.output.output_files)
            right_outputs = set(
                Path(p) for p in right_record.command.output.output_files)
            common_outputs = left_outputs & right_outputs
            for p in common_outputs:
                visited_outputs.add(p)


def setup_logdir_for_logdump(path: Path, verbose: bool = False) -> Path:
    """Automatically setup a log dir for logdump, if needed.

    The `logdump` tool expects a log directory, but sometimes we
    are only given a log file.  When we are given a log file, copy it
    to a directory.  The created directory is cached by the hash
    of the log file, so that it can be re-used for multiple queries.

    Args:
      path: path to either a .rrpl/.rpl log, or an reproxy log dir
        containing a .rrpl/.rpl log.

    Returns:
      The original path, or path to a newly created cached directory
      containing a copy of the log.
    """
    if path.is_dir():
        # This is a directory containing a .rrpl or .rpl file.
        return path

    # Otherwise, this is assumed to be a .rrpl or .rpl file.
    # Copy it to a cached location.
    tmpdir = Path(os.environ.get('TMPDIR', '/tmp'))
    with open(path, "rb") as f:
        contents = f.read()
    readable_hash = hashlib.sha256(contents).hexdigest()
    cached_log_dir = tmpdir / 'action_diff_py' / 'cache' / readable_hash
    if verbose:
        msg(f'Copying log to {cached_log_dir} for logdump processing.')
    cached_log_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, cached_log_dir)
    return cached_log_dir


def _log_dump_from_pb(log_pb_file: Path) -> log_pb2.LogDump:
    log_dump = log_pb2.LogDump()
    with open(log_pb_file, mode='rb') as logf:
        log_dump.ParseFromString(logf.read())
    return log_dump


def convert_reproxy_actions_log(
        reproxy_logdir: Path,
        reclient_bindir: Path,
        verbose: bool = False) -> log_pb2.LogDump:
    log_pb_file = reproxy_logdir / "reproxy_log.pb"

    if not log_pb_file.exists():
        # Convert reproxy text logs to binary proto.
        logdump_cmd = [
            str(reclient_bindir / "logdump"),
            "--proxy_log_dir",
            str(reproxy_logdir),
            "--output_dir",
            str(reproxy_logdir),  # Use same log dir, must be writeable.
        ]
        if verbose:
            msg(f"Ingesting logs from {reproxy_logdir}.")
        # logdump could take a few minutes on large reproxy logs.
        subprocess.check_call(logdump_cmd)
        # Produces "reproxy_log.pb" in args.reproxy_logdir.

    if verbose:
        msg(f"Loading log records from {log_pb_file}.")

    return _log_dump_from_pb(log_pb_file)


def parse_log(
        log_path: Path,
        reclient_bindir: Path,
        verbose: bool = False) -> ReproxyLog:
    """Prepare and parse reproxy logs."""
    return ReproxyLog(
        convert_reproxy_actions_log(
            reproxy_logdir=setup_logdir_for_logdump(log_path, verbose=verbose),
            reclient_bindir=reclient_bindir,
            verbose=verbose,
        ))


def _action_digest_eq(
        left: log_pb2.LogRecord, right: log_pb2.LogRecord) -> bool:
    return left.remote_metadata.action_digest == right.remote_metadata.action_digest


def _diff_printer(log) -> str:
    lines = [f'action_digest: {log.remote_metadata.action_digest}']
    for file, digest in log.remote_metadata.output_file_digests.items():
        lines.extend([
            f'file: {file}',
            f'  digest: {digest}',
        ])
    return '\n'.join(lines)


# Defined at the module level for multiprocessing to be able to serialize.
def _process_log_mp(log_path: Path) -> ReproxyLog:
    """Prepare and parse reproxy logs (for parallel processing)."""
    return parse_log(
        log_path=log_path,
        reclient_bindir=fuchsia.RECLIENT_BINDIR,
        verbose=True,
    )


def parse_logs(reproxy_logs: Sequence[Path]) -> Sequence[ReproxyLog]:
    """Parse reproxy logs, maybe in parallel."""
    try:
        with multiprocessing.Pool() as pool:
            return pool.map(_process_log_mp, reproxy_logs)
    except OSError:  # in case /dev/shm is not write-able (required)
        if len(reproxy_logs) > 1:
            msg("Warning: downloading sequentially instead of in parallel.")
        return map(_process_log_mp, reproxy_logs)


def diff_logs(args: argparse.Namespace) -> int:
    logs = parse_logs(args.reproxy_logs)

    # len(logs) == 2, enforced by _MAIN_ARG_PARSER.

    # TODO: argparse options to customize:
    #   different comparators
    #   different result printers
    # TODO: use a structured proto difference representation
    #   to avoid showing fields that match.
    left, right = logs
    diffs = list(left.diff_by_outputs(right, _action_digest_eq))

    if diffs:
        print('Action differences found.')
        for path, d_left, d_right in diffs:
            left_text = _diff_printer(d_left)
            right_text = _diff_printer(d_right)
            print('---------------------------------')
            print(f'{path}: (left)\n{left_text}\n')
            print(f'{path}: (right)\n{right_text}\n')

    return 0


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()

    # universal options
    parser.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Print additional debug information while running.",
    )

    subparsers = parser.add_subparsers(required=True)
    diff_parser = subparsers.add_parser('diff', help='diff --help')
    diff_parser.set_defaults(func=diff_logs)
    diff_parser.add_argument(
        "reproxy_logs",
        type=Path,
        help="reproxy logs (.rrpl or .rpl) or the directories containing them",
        metavar="PATH",
        nargs=2,
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def main(argv: Sequence[str]) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
