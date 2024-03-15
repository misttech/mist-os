# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""RTC conformance test."""

import datetime
import logging
import random

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts, test_runner

LOGGER: logging.Logger = logging.getLogger(__name__)


class RtcTest(fuchsia_base_test.FuchsiaBaseTest):
    """fuchsia.hardware.rtc.Device protocol conformance Test."""

    def setup_class(self) -> None:
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def setup_test(self):
        super().setup_test()
        self.rtc = self.dut.rtc

    def test_rtc(self) -> None:
        """Test the fuchsia.hardware.rtc.Device protocol.

        This test verifies that the RTC can be written to, read from, and
        re-read post-soft-reset (any reboot which doesn't cut power to the
        chip). When re-reading the time off the RTC, the test verifies the time
        read is within some threshold of the expected time.
        """
        threshold = 5  # Seconds.

        LOGGER.info("Starting RTC conformance test on %s", self.dut.device_name)

        # We'll assume a random year just so that subsequent test invocations do
        # not contain overlapping testing conditions.
        randyear = random.randint(1900, 2099)

        # We'll start by setting the RTC to a known time: YEAR-12-20T23:30:00.
        #
        # The values for month, day, and h/m/s are chosen to try and catch any
        # possible field transposition errors in the driver.
        base_time = datetime.datetime(randyear, 12, 20, 23, 30, 0)
        LOGGER.info("Setting RTC time: %s", base_time)
        self.rtc.set(base_time)

        # Ensure the time was actually set by re-reading the time and ensuring
        # the time elapsed is within some reasonable threshold. The threshold
        # value may need tuning. The value read here will be used as a benchmark
        # later (post-reboot).
        rtc_time1 = self.rtc.get()
        LOGGER.info("Time read off RTC is: %s", rtc_time1)
        asserts.assert_less(
            rtc_time1 - base_time, datetime.timedelta(seconds=threshold)
        )

        # Next, reboot the device, re-read the RTC time, and (again) ensure the
        # total elapsed time is within some reasonable threshold. This needs to
        # account for the time spent actually rebooting the device.
        #
        # When reading the current time, microseconds are discarded because the
        # RTC protocol only has 1s resolution. Otherwise, the unaccounted-for
        # time would push the delta slightly negative, which is rendered in a
        # bizarre format (e.g. -1 day 23:59:59.600 vs. -00:00:00.400 given
        # a 400ms error).
        #
        # See:
        # https://docs.python.org/3/library/datetime.html#datetime.timedelta.resolution
        start = datetime.datetime.now().replace(microsecond=0)
        self.dut.reboot()
        elapsed = datetime.datetime.now().replace(microsecond=0) - start

        LOGGER.info("Elapsed time spent during reboot: %s", elapsed)

        rtc_time2 = self.rtc.get()
        LOGGER.info("Expected RTC time %s", rtc_time1 + elapsed)
        LOGGER.info("Got RTC time: %s", rtc_time2)

        # Here, we take the benchmark time above, subtract the time spent
        # rebooting, and then subtract the current time. The delta value should
        # be close to 0.
        delta = rtc_time2 - rtc_time1 - elapsed
        LOGGER.info("Delta: %s", delta)
        asserts.assert_less(delta, datetime.timedelta(seconds=threshold))


if __name__ == "__main__":
    test_runner.main()
