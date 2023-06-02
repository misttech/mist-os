#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

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

        with mock.patch.object(subprocess, "check_call") as mock_process_call:
            with mock.patch.object(__builtins__, "open") as mock_open:
                with mock.patch.object(log_pb2.LogDump,
                                       "ParseFromString") as mock_parse:
                    log_dump = reproxy_logs.convert_reproxy_actions_log(
                        reproxy_logdir=Path("/tmp/reproxy.log.dir"),
                        reclient_bindir=Path("/usr/local/reclient/bin"),
                    )
        mock_process_call.assert_called_once()
        mock_open.assert_called_once()
        mock_parse.assert_called_once()
        self.assertEqual(log_dump, log_pb2.LogDump())


if __name__ == '__main__':
    unittest.main()
