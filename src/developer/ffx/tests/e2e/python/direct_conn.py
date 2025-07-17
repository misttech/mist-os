#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import json
import logging

import ffxtestcase
from honeydew.transports.ffx.errors import FfxCommandError
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxDirectTest(ffxtestcase.FfxTestCase):
    """FFX host tool E2E test For Direct connections."""

    def setup_test(self) -> None:
        """Each test must run without the daemon."""
        super().setup_test()
        self.dut.ffx.run(["daemon", "stop"])

    def test_direct_target_echo_no_start_daemon(self) -> None:
        """Test `ffx --direct target echo` does not affect daemon state."""
        output = self.run_ffx(
            [
                "--direct",
                "--machine",
                "json",
                "target",
                "echo",
                "From a Test",
            ],
        )

        out = json.loads(output)
        asserts.assert_equal(out["message"], "From a Test")
        with asserts.assert_raises(FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])


if __name__ == "__main__":
    test_runner.main()
