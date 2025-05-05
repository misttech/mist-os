# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Tests the firmware's fastboot capabilities."""

import logging
import re
from typing import Tuple

from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from mobly import asserts, test_runner

# Required fastboot variables.
# - key: variable name
# - value: a list of acceptable strings for the value, or None for any value
_REQUIRED_VARS = {
    # Required for fastboot protocol communication.
    "max-download-size": None,
    "serialno": None,
    # Required for `ffx target flash` to identify boards.
    "hw-revision": None,
    # Slot information for debugging and failure analysis.
    "current-slot": ["a", "b"],
    "slot-count": ["2"],
    "slot-retry-count:a": None,
    "slot-retry-count:b": None,
    "slot-successful:a": None,
    "slot-successful:b": None,
    "slot-unbootable:a": None,
    "slot-unbootable:b": None,
    # TODO(b/308030836):: add is-userspace back after all devices support it.
    # "is-userspace": ["no"],
    # TODO(b/308030836):: add the vx- variables after all devices support it.
    # "vx-locked": ["no", "yes"],
    # "vx-unlockable": ["ephemeral", "no", "yes"],
}

# Regexp to extract variable name and value from fastboot output.
#
# Example getvar output:
# - "foo:bar"
# - "foo: bar"
# - "foo:a: bar"
# - "foo: bar baz"
# - "(bootloader) foo: bar"
#
# TODO(b/308030836): require a space between the name/arg and value, otherwise
# parsing can be ambiguous if the value itself could contain a colon. Currently
# we assume there is at most one arg per variable, and any other colon belongs
# to the value, e.g. `foo:a:b:c` results in name="foo:a", value="b:c".
_GETVAR_REGEX = re.compile(
    r"(\(bootloader\) )?(?P<name>[a-z\-]+(:[a-z0-9]+)?): ?(?P<val>.*)"
)


def parse_getvar(line: str) -> Tuple[str, str]:  # type: ignore[return]
    """Parses a `getvar` or `getvar all` output line.

    Args:
        line: a single line of `getvar` output.

    Returns:
        A tuple containing (name, value).

    Raises:
        Mobly assert if the line doesn't look like `getvar` output.
    """
    match = _GETVAR_REGEX.match(line)
    if match:
        return (match.group("name"), match.group("val"))
    asserts.fail("Failed to parse getvar output", extras={"line": line})


class FastbootTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        """Initializes all DUT(s)"""
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        # TODO(http://b/276740268#comment33): add support for rebooting into
        # fastboot here and leaving the device in fastboot mode for the entire
        # test class?
        #
        # For the time being we'll group tests that would ideally be separate
        # into single methods to minimize the number of times we have to reboot.

    def setup_test(self) -> None:
        """Puts the device into fastboot mode before each test."""
        super().setup_test()
        self.device.fastboot.boot_to_fastboot_mode()

    def teardown_test(self) -> None:
        """Puts the device back into Fuchsia mode after each test."""
        if self.device.fastboot.is_in_fastboot_mode():
            self.device.fastboot.boot_to_fuchsia_mode()
        super().teardown_test()

    def test_getvar(self) -> None:
        """Tests fastboot variables."""
        # Make sure each variable can also be individually queried and that the
        # value is what we expect.
        logging.info("Checking `getvar` variables")
        for expected_name, expected_values in _REQUIRED_VARS.items():
            output = self.device.fastboot.run(["getvar", expected_name])
            asserts.assert_equal(
                len(output),
                1,
                "getvar output should only be a single line",
                extras={"output": output},
            )
            name, value = parse_getvar(output[0])

            asserts.assert_equal(
                name, expected_name, "getvar name doesn't match expected"
            )
            if expected_values:
                asserts.assert_in(
                    value,
                    expected_values,
                    "getvar value doesn't match expected",
                    extras={"name": name},
                )

        # Make sure all the variables we care about also exist in `getvar all`.
        logging.info("Checking `getvar all`")
        getvar_all_vars = []
        for line in self.device.fastboot.run(["getvar", "all"]):
            name, _ = parse_getvar(line)
            if name != "all":
                getvar_all_vars.append(name)

        for name in _REQUIRED_VARS:
            asserts.assert_in(
                name, getvar_all_vars, "Missing variable in `getvar all`"
            )


if __name__ == "__main__":
    test_runner.main()
