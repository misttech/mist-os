#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functions for working with reproxy logs.

This requires python protobufs for reproxy LogDump.
"""

import hashlib
import os
import shutil
import subprocess

from api.proxy import log_pb2
from pathlib import Path
from typing import Dict

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

    log_dump = log_pb2.LogDump()
    with open(log_pb_file, mode='rb') as logf:
        log_dump.ParseFromString(logf.read())

    return log_dump
