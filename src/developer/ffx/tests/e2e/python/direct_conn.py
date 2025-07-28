#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
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
    """FFX host tool E2E test for Direct connections."""

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

    def test_direct_target_echo(self) -> None:
        """Test `ffx --direct target echo` does not affect daemon state."""
        out = self._run_ffx_direct(
            [
                "target",
                "echo",
                "From a Test",
            ],
        )

        asserts.assert_equal(out["message"], "From a Test")

    def test_direct_target_list(self) -> None:
        """Test `ffx --direct target list` does not affect daemon state."""
        out = self._run_ffx_direct(
            [
                "target",
                "list",
                f"{self.dut_ssh_address}",
            ],
        )
        asserts.assert_equal(out[0]["target_state"], "Product")
        asserts.assert_equal(out[0]["rcs_state"], "Y")

    def test_direct_version(self) -> None:
        """Test `ffx --direct version -v` does not query the daemon."""
        out = self._run_ffx_direct(
            [
                "version",
                "-v",
            ],
        )
        asserts.assert_is_none(out["daemon_version"])
        asserts.assert_is_not_none(out["tool_version"])

    # This tests that an arbitrary tool using RCS, that was not specifically
    # modified to support direct mode, does on fact work in direct mode.
    def test_component_list(self) -> None:
        """Test `ffx --direct component list` does not query the daemon."""
        out = self._run_ffx_direct(
            [
                "component",
                "list",
            ],
        )
        asserts.assert_greater(len(out["instances"]), 0)

    # This tests that a tool that uses non-RCS proxies works in direct mode.
    def test_repo_list(self) -> None:
        """Test `ffx --direct target repository list` does not query the daemon."""
        out = self._run_ffx_direct(
            [
                "target",
                "repository",
                "list",
            ],
        )
        asserts.assert_greater(len(out["ok"]["data"]), 0)

    # This tests that a tool that uses a non-standard target connection flow
    # works in direct mode.
    def test_ffx_log(self) -> None:
        """Test `ffx --direct log` does not query the daemon."""
        # Can't run with _run_ffx_direct() because the output is not
        # valid JSON; instead each line is a JSON object.
        out = self.run_ffx(
            [
                "--direct",
                "--machine",
                "json",
                "log",
                "--symbolize",
                "off",
                "--since-boot",
                "1",
                "--until-boot",
                "2",
            ],
        )
        # We can't actually guarantee that we get valid JSON from `ffx log`,
        # but we can note when we don't.
        try:
            logs = [json.loads(l) for l in out.strip().split("\n")]
            asserts.assert_greater(len(logs), 0)
        except json.decoder.JSONDecodeError:
            _LOGGER.info(f"Got bad JSON from ffx-log: {repr(out[:100])}")


if __name__ == "__main__":
    test_runner.main()
