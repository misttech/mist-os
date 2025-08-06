#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import logging

import ffxtestcase
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxDirectDaemonTest(ffxtestcase.FfxTestCase):
    """FFX host tool E2E test for daemon subtools when in direct mode."""

    def setup_class(self) -> None:
        # This just gets some things out of the way before we start turning
        # the daemon off and on again.
        super().setup_class()
        self.dut_ssh_address = self.dut.ffx.get_target_ssh_address()

    def test_direct_daemon_disconnect(self) -> None:
        """Test `ffx --direct daemon disconnect` does not raise an exception."""
        self.run_ffx(
            [
                "--direct",
                "daemon",
                "disconnect",
            ],
        )


if __name__ == "__main__":
    test_runner.main()
