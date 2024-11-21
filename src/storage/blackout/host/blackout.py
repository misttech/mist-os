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

import asyncio
import logging
import random

import fidl.fuchsia_blackout_test as blackout
import honeydew.errors
import honeydew.utils.common
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing.custom_types import FidlEndpoint
from mobly import asserts, test_runner
from test_case_revive import test_case_revive

_LOGGER = logging.getLogger(__name__)


class BlackoutTest(test_case_revive.TestCaseRevive):
    def setup_class(self) -> None:
        super().setup_class()

        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        asserts.abort_class_if(
            "component_name" not in self.user_params, "Missing component name!"
        )
        asserts.abort_class_if(
            "component_url" not in self.user_params, "Missing component url!"
        )
        self.component_name = self.user_params["component_name"]
        self.component_url = self.user_params["component_url"]

        self.device_label = self.user_params.get("device_label", "default-test")
        self.device_path = self.user_params.get("device_path")
        self.load_generation_duration = self.user_params.get(
            "load_generation_duration", 0
        )
        self.destroy_after_test = self.user_params.get(
            "destroy_after_test", False
        )
        self.seed = self.user_params.get("seed", random.randint(0, 1024 * 1024))
        _LOGGER.info(f"Blackout: random seed is {self.seed}")

        self.create_blackout_component()

    def teardown_class(self) -> None:
        try:
            self.dut.ffx.run(
                [
                    "component",
                    "stop",
                    self.component_name,
                ]
            )
        except honeydew.errors.FfxCommandError:
            _LOGGER.warning(
                "Blackout: Failed to stop component during teardown"
            )
        super().teardown_class()

    def setup_test(self) -> None:
        super().setup_test()
        _LOGGER.info("Blackout: Setting up test filesystem")
        res = asyncio.run(
            self.blackout_proxy.setup(
                device_label=self.device_label,
                device_path=self.device_path,
                seed=self.seed,
            )
        )
        asserts.assert_equal(res.err, None, "Failed to run setup")
        _LOGGER.info("Blackout: Running filesystem load")
        res = asyncio.run(
            self.blackout_proxy.test(
                device_label=self.device_label,
                device_path=self.device_path,
                seed=self.seed,
                duration=self.load_generation_duration,
            )
        )
        asserts.assert_equal(res.err, None, "Failed to run load generation")
        if self.destroy_after_test:
            _LOGGER.info("Blackout: destroying test component instance")
            self.dut.ffx.run(
                [
                    "component",
                    "destroy",
                    self.component_name,
                ]
            )

    def create_blackout_component(self) -> None:
        # TODO(https://fxbug.dev/340586785): sometimes this fails. Until it becomes more stable (or
        # the retry logic is put into the framework), retry it for a bit.
        honeydew.utils.common.retry(
            lambda: self.dut.ffx.run(
                [
                    "component",
                    "run",
                    "--recreate",
                    self.component_name,
                    self.component_url,
                ]
            ),
            timeout=60,
            wait_time=5,
        )
        ch = self.dut.fuchsia_controller.connect_device_proxy(
            FidlEndpoint(self.component_name, blackout.Controller.MARKER)
        )
        self.blackout_proxy = blackout.Controller.Client(ch)

    @test_case_revive.tag_test(
        tag_name="revive_test_case",
        test_method_execution_frequency=test_case_revive.TestMethodExecutionFrequency.POST_ONLY,
    )
    def _test_do_verification(self) -> None:
        self.create_blackout_component()
        _LOGGER.info("Blackout: Running device verification")
        res = asyncio.run(
            self.blackout_proxy.verify(
                device_label=self.device_label,
                device_path=self.device_path,
                seed=self.seed,
            )
        )
        asserts.assert_equal(
            res.err,
            None,
            "Verification Failure! Filesystem is likely corrupt!!",
        )


if __name__ == "__main__":
    test_runner.main()
