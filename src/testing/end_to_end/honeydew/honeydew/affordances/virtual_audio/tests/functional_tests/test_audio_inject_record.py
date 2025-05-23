# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Audio affordance."""

import asyncio
import logging
import time

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.fuchsia_device import fuchsia_device

_AUDIO_FILE_INPUT = "audio_runtime_deps/sine-wave.wav"
_LOGGER: logging.Logger = logging.getLogger(__name__)


class AudioAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Audio affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def setup_test(self) -> None:
        super().setup_test()

    def teardown_test(self) -> None:
        super().teardown_test()

    def test_audio(self) -> None:
        responseAudio = self.device.virtual_audio.capture()
        inputResponse = self.device.virtual_audio.inject(_AUDIO_FILE_INPUT)

        asyncio.run(inputResponse.wait_until_injection_is_done())

        time.sleep(5)

        data = asyncio.run(responseAudio.stop_and_extract_response())
        _LOGGER.info("Got %d bytes", len(data))


if __name__ == "__main__":
    test_runner.main()
