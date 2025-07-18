#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import json
import logging
from typing import Any, List

import ffxtestcase
from honeydew.transports.ffx.errors import FfxCommandError
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxDirectTest(ffxtestcase.FfxTestCase):
    """FFX host tool E2E test For Direct connections."""

    def setup_class(self) -> None:
        # This just gets some things out of the way before we start turning
        # the daemon off and on again.
        super().setup_class()
        self.dut_ssh_address = self.dut.ffx.get_target_ssh_address()

    def setup_test(self) -> None:
        """Each test must run without the daemon."""
        super().setup_test()
        self.dut.ffx.run(["daemon", "stop"])

    def teardown_test(self) -> None:
        # Verify that we did not start the daemon
        with asserts.assert_raises(FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])
        super().teardown_test()

    # Run the ffx command with the --direct arg, and parse the results
    def _run_ffx_direct(self, cmd: List[str]) -> Any:
        all_args = [
            "--direct",
            "--machine",
            "json",
        ]
        all_args += cmd
        return json.loads(self.run_ffx(all_args))

    def test_direct_target_echo_no_start_daemon(self) -> None:
        """Test `ffx --direct target echo` does not affect daemon state."""
        out = self._run_ffx_direct(
            [
                "target",
                "echo",
                "From a Test",
            ],
        )

        asserts.assert_equal(out["message"], "From a Test")

    def test_direct_target_list_no_start_daemon(self) -> None:
        """Test `ffx --direct target list` does not affect daemon state."""
        out = self._run_ffx_direct(
            [
                "target",
                "list",
                f"{self.dut_ssh_address}",
            ],
        )
        asserts.assert_equal(out[0]["target_state"], "Product")


if __name__ == "__main__":
    test_runner.main()
