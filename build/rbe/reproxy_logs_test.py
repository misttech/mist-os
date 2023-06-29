#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import shutil
import subprocess
import tempfile
import unittest

from unittest import mock

from api.proxy import log_pb2

import reproxy_logs

from pathlib import Path


def _write_file_contents(path: Path, contents: str):
    with open(path, 'w') as f:
        f.write(contents)


def _digests_equal(left: log_pb2.LogRecord, right: log_pb2.LogRecord) -> bool:
    return left.remote_metadata.action_digest == right.remote_metadata.action_digest


class ReproxyLogTests(unittest.TestCase):

    def test_construction(self):
        record1 = log_pb2.LogRecord()
        record1.command.output.output_files.extend(['obj/foo.o', 'obj/foo2.o'])
        record1.remote_metadata.action_digest = 'ffff0000/13'
        record2 = log_pb2.LogRecord()
        record2.command.output.output_files.extend(['obj/bar.o'])
        record2.remote_metadata.action_digest = 'bbbbaaaa/9'
        log_dump = log_pb2.LogDump(records=[record1, record2])
        log = reproxy_logs.ReproxyLog(log_dump)
        self.assertEqual(log.proto, log_dump)
        self.assertEqual(
            log.records_by_output_file, {
                Path('obj/foo.o'): record1,
                Path('obj/foo2.o'): record1,
                Path('obj/bar.o'): record2,
            })
        self.assertEqual(
            log.records_by_action_digest, {
                'ffff0000/13': record1,
                'bbbbaaaa/9': record2,
            })

    def test_diff_by_outputs_matching(self):
        record1 = log_pb2.LogRecord()
        record1.command.output.output_files.extend(['obj/foo.o'])
        record1.remote_metadata.action_digest = 'ffeeff001100/313'
        log_dump = log_pb2.LogDump(records=[record1])
        log = reproxy_logs.ReproxyLog(log_dump)
        diffs = list(log.diff_by_outputs(log, _digests_equal))
        self.assertEqual(diffs, [])

    def test_diff_by_outputs_different(self):
        output = Path('obj/foo')
        record1 = log_pb2.LogRecord()
        record1.command.output.output_files.extend([str(output)])
        record1.remote_metadata.action_digest = 'ffeeff001100/313'
        record2 = log_pb2.LogRecord()
        record2.command.output.output_files.extend([str(output)])
        record2.remote_metadata.action_digest = '98231772bbbd/313'
        log_dump1 = log_pb2.LogDump(records=[record1])
        log_dump2 = log_pb2.LogDump(records=[record2])
        log1 = reproxy_logs.ReproxyLog(log_dump1)
        log2 = reproxy_logs.ReproxyLog(log_dump2)
        diffs = list(log1.diff_by_outputs(log2, _digests_equal))
        self.assertEqual(diffs, [(output, record1, record2)])


class SetupLogdirForLogDumpTest(unittest.TestCase):

    def test_already_a_directory(self):
        with tempfile.TemporaryDirectory() as td:
            reproxy_logs.setup_logdir_for_logdump(Path(td))

    def test_log_file_copied_to_new_dir(self):
        with tempfile.TemporaryDirectory() as td:
            log_file = Path(td) / 'test.rrpl'
            log_contents = 'fake log contents, not checked'
            _write_file_contents(log_file, log_contents)
            with mock.patch.object(Path, 'mkdir') as mock_mkdir:
                with mock.patch.object(shutil, 'copy2') as mock_copy:
                    new_log_dir = reproxy_logs.setup_logdir_for_logdump(
                        log_file)
            mock_mkdir.assert_called_with(parents=True, exist_ok=True)
            mock_copy.assert_called_with(log_file, new_log_dir)


class ConvertReproxyActionsLogTest(unittest.TestCase):

    def test_basic(self):
        empty_log = log_pb2.LogDump()
        with mock.patch.object(subprocess, "check_call") as mock_process_call:
            with mock.patch.object(reproxy_logs, "_log_dump_from_pb",
                                   return_value=empty_log) as mock_parse:
                log_dump = reproxy_logs.convert_reproxy_actions_log(
                    reproxy_logdir=Path("/tmp/reproxy.log.dir"),
                    reclient_bindir=Path("/usr/local/reclient/bin"),
                )
        mock_process_call.assert_called_once()
        mock_parse.assert_called_once()
        self.assertEqual(log_dump, log_dump)


class DiffLogsTests(unittest.TestCase):

    def test_flow(self):
        empty_log = log_pb2.LogDump()
        empty_logs = [reproxy_logs.ReproxyLog(empty_log)] * 2
        args = argparse.Namespace(reproxy_logs=[Path('log1'), Path('log2')],)
        with mock.patch.object(reproxy_logs, 'parse_logs',
                               return_value=empty_logs) as mock_multi_parse:
            status = reproxy_logs.diff_logs(args)
        self.assertEqual(status, 0)


class MainTests(unittest.TestCase):

    def test_diff(self):
        empty_log = log_pb2.LogDump()
        empty_logs = [reproxy_logs.ReproxyLog(empty_log)] * 2
        with mock.patch.object(reproxy_logs, 'diff_logs',
                               return_value=0) as mock_diff:
            with mock.patch.object(reproxy_logs, 'parse_logs',
                                   return_value=empty_logs) as mock_multi_parse:
                status = reproxy_logs.main(['diff', 'left.rrpl', 'right.rrpl'])
        self.assertEqual(status, 0)


if __name__ == '__main__':
    unittest.main()
