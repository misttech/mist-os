#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import subprocess
import unittest
from unittest import mock

from api.proxy import log_pb2

import reproxy_logs

from pathlib import Path

class ReproxyLogTests(unittest.TestCase):
    def test_construction(self):
        record1 = log_pb2.LogRecord()
        record1.command.output.output_files.extend(['obj/foo.o'])
        record2 = log_pb2.LogRecord()
        record2.command.output.output_files.extend(['obj/bar.o'])
        log_dump = log_pb2.LogDump(records=[record1, record2])
        log = reproxy_logs.ReproxyLog(log_dump)
        self.assertEqual(log.proto, log_dump)
        self.assertEqual(log.records_by_output_file,
                         {Path('obj/foo.o'): record1,
                          Path('obj/bar.o'): record2,
                          })

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
