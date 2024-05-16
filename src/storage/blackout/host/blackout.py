#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Blackout - a power failure test for the filesystems.

This is a general host-side harness for all blackout tests. The params_source file passed in the
build target should specify what component it's running, what collection it should be put into, and
the block device label (and optionally path) to run on.
"""

import logging
import asyncio
import time

import fidl.fuchsia_blackout_test as blackout

from test_case_revive import test_case_revive
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.custom_types import FidlEndpoint
import honeydew.errors
import honeydew.utils.common

_LOGGER = logging.getLogger(__name__)


class BlackoutTest(test_case_revive.TestCaseRevive):
    def setup_class(self) -> None:
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.create_blackout_component()
        self.dut.register_for_on_device_boot(fn=self.create_blackout_component)

    def teardown_class(self) -> None:
        self.dut.ffx.run(
            [
                "component",
                "stop",
                self.user_params["component_name"],
            ]
        )
        super().teardown_class()

    def setup_test(self) -> None:
        super().setup_test()
        _LOGGER.info("Blackout: Setting up test filesystem")
        res = asyncio.run(
            self.blackout_proxy.setup(
                device_label=self.user_params["device_label"],
                device_path=self.user_params["device_path"],
                seed=1234,
            )
        )
        asserts.assert_equal(res.err, None, "Failed to run setup")
        _LOGGER.info("Blackout: Running filesystem load")
        res = asyncio.run(
            self.blackout_proxy.test(
                device_label=self.user_params["device_label"],
                device_path=self.user_params["device_path"],
                seed=1234,
                duration=self.user_params["test_duration"],
            )
        )
        asserts.assert_equal(res.err, None, "Failed to run load generation")

    def create_blackout_component(self) -> None:
        # TODO(https://fxbug.dev/340586785): sometimes this fails. Until it becomes more stable (or
        # the retry logic is put into the framework), retry it for a bit.
        honeydew.utils.common.retry(
            lambda: self.dut.ffx.run(
                [
                    "component",
                    "run",
                    "--recreate",
                    self.user_params["component_name"],
                    self.user_params["component_url"],
                ]
            ),
            timeout=60,
            wait_time=5,
        )
        ch = self.dut.fuchsia_controller.connect_device_proxy(
            FidlEndpoint(
                self.user_params["component_name"], blackout.Controller.MARKER
            )
        )
        self.blackout_proxy = blackout.Controller.Client(ch)

    @test_case_revive.tag_test(
        tag_name="revive_test_case",
        test_method_execution_frequency=test_case_revive.TestMethodExecutionFrequency.POST_ONLY,
    )
    def _test_do_verification(self) -> None:
        _LOGGER.info("Blackout: Running device verification")
        res = asyncio.run(
            self.blackout_proxy.verify(
                device_label=self.user_params["device_label"],
                device_path=self.user_params["device_path"],
                seed=1234,
            )
        )
        asserts.assert_equal(
            res.err,
            None,
            "Verification Failure! Filesystem is likely corrupt!!",
        )


if __name__ == "__main__":
    test_runner.main()
