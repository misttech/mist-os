#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Functions for working with reproxy logs.

This requires python protobufs for reproxy LogDump.
"""

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
      self._proto : log_pb2.LogDump = log_dump

      self._records_by_output_file = self._index_records_by_output_file()

  def _index_records_by_output_file(self) -> Dict[Path, log_pb2.LogRecord]:
      """Map files to the records of the actions that created them."""
      records : Dict[Path, log_pb2.LogRecord] = {}
      for record in self.proto.records:
          for output_file in record.command.output.output_files:
              records[Path(output_file)] = record
      return records

  @property
  def proto(self) -> log_pb2.LogDump:
      return self._proto

  @property
  def records_by_output_file(self) -> Dict[Path, log_pb2.LogRecord]:
      return self._records_by_output_file

def convert_reproxy_actions_log(
    reproxy_logdir: Path, reclient_bindir: Path, verbose: bool = False) -> log_pb2.LogDump:
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

