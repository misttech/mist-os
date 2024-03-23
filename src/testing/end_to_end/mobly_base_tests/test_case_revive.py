# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Lacewing Test Case Reviver."""

import importlib

import enum
import logging

from collections.abc import Callable
from typing import Any

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.auxiliary_devices import power_switch

_LOGGER: logging.Logger = logging.getLogger(__name__)


_DMC_MODULE: str = "honeydew.auxiliary_devices.power_switch_dmc"
_DMC_CLASS: str = "PowerSwitchDmc"


class FuchsiaDeviceOperation(enum.StrEnum):
    """Operation that need to be performed on Fuchsia Device."""

    NONE = "None"

    SOFT_REBOOT = "Soft-Reboot"

    HARD_REBOOT = "Hard-Reboot"

    POWER_CYCLE = "Power-Cycle"

    IDLE_SUSPEND_AUTO_RESUME = "Idle-Suspend-Auto-Resume"


def tag_test(tag_name: str = "revive_test_case") -> Callable[..., Any]:
    """Decorator that can be used to tag a test with a label"""

    def tags_decorator(func: Callable[..., Any]) -> Callable[..., Any]:
        # pylint: disable=protected-access
        func._tag = tag_name  # type: ignore[attr-defined]
        return func

    return tags_decorator


class TestCaseRevive(fuchsia_base_test.FuchsiaBaseTest):
    """Test case revive is a lacewing test class that takes any Lacewing test
    case and modifies it to run in below sequence:

    1. Run the test case
    2. Perform an operation requested by user (from list of supported operations)
    3. Rerun the test case
    """

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""

        self._test_case_revive: bool = self.user_params.get(
            "test_case_revive", False
        )
        if self._test_case_revive is False:
            _LOGGER.info(
                "[TestCaseRevive] - test_case_revive setting is not enabled "
                "in user_params. So not testing in revive mode...",
            )
            return

        self._fuchsia_device_operation: str = self.user_params.get(
            "fuchsia_device_operation", FuchsiaDeviceOperation.NONE
        )
        try:
            self._fuchsia_device_operation_obj: FuchsiaDeviceOperation = (
                FuchsiaDeviceOperation(self._fuchsia_device_operation)
            )
        except ValueError as err:
            raise ValueError(
                f"'{self._fuchsia_device_operation}' operation is not "
                f"supported by 'TestCaseRevive'"
            ) from err

        _LOGGER.info(
            "[TestCaseRevive] - test_case_revive setting is enabled in "
            "user_params. So testing in revive mode with '%s' operation...",
            self._fuchsia_device_operation,
        )

        test_cases: list[str] = [
            attribute
            for attribute in dir(self)
            if callable(getattr(self, attribute))
            and attribute.startswith("test_") is True
        ]
        _LOGGER.info(
            "[TestCaseRevive] - List of all the test cases in this test "
            "class: %s",
            test_cases,
        )

        # pylint: disable=protected-access
        revived_test_cases: list[str] = [
            test_case
            for test_case in test_cases
            if "_tag" in dir(getattr(self, test_case))
            and getattr(self, test_case)._tag == "revive_test_case"
        ]
        _LOGGER.info(
            "[TestCaseRevive] - List of all the test cases in this test class "
            "that are configured to run with revived sequence: %s",
            revived_test_cases,
        )

        test_arg_tuple_list: list[tuple[str]] = [
            (revived_test_case,) for revived_test_case in revived_test_cases
        ]
        self.generate_tests(
            test_logic=self._test_case_revive_logic,
            name_func=self._revived_test_case_name_func,
            arg_sets=test_arg_tuple_list,
        )

    def _perform_op(self) -> None:
        """Perform user specified operation"""

        for fuchsia_device in self.fuchsia_devices:
            if (
                self._fuchsia_device_operation_obj
                == FuchsiaDeviceOperation.IDLE_SUSPEND_AUTO_RESUME
            ):
                fuchsia_device.system_power_state_controller.idle_suspend_auto_resume()
            elif (
                self._fuchsia_device_operation_obj
                == FuchsiaDeviceOperation.SOFT_REBOOT
            ):
                fuchsia_device.reboot()
            elif self._fuchsia_device_operation_obj in [
                FuchsiaDeviceOperation.HARD_REBOOT,
                FuchsiaDeviceOperation.POWER_CYCLE,
            ]:
                _LOGGER.debug(
                    "[TestCaseRevive] - Importing %s.%s module",
                    _DMC_MODULE,
                    _DMC_CLASS,
                )
                power_switch_class: power_switch.PowerSwitch = getattr(
                    importlib.import_module(_DMC_MODULE), _DMC_CLASS
                )

                _LOGGER.debug(
                    "[TestCaseRevive] - Instantiating %s.%s module",
                    _DMC_MODULE,
                    _DMC_CLASS,
                )
                self._power_switch: power_switch.PowerSwitch = (
                    power_switch_class(device_name=fuchsia_device.device_name)
                )

                fuchsia_device.power_cycle(
                    power_switch=self._power_switch, outlet=None
                )

    def _test_case_revive_logic(self, test_case: str) -> None:
        """TestCaseRevive logic"""
        _LOGGER.info(
            "[TestCaseRevive] - Running the %s before performing %s operation...",
            test_case,
            self._fuchsia_device_operation_obj,
        )
        getattr(self, test_case)()

        _LOGGER.info(
            "[TestCaseRevive] - Performing %s operation on all Fuchsia devices "
            "that are part of the testbed...",
            self._fuchsia_device_operation_obj,
        )
        self._perform_op()

        _LOGGER.info(
            "[TestCaseRevive] - Running the %s after performing %s operation...",
            test_case,
            self._fuchsia_device_operation_obj,
        )
        getattr(self, test_case)()

    def _revived_test_case_name_func(self, test_case: str) -> str:
        """Revived test case name function"""
        return f"{test_case}_revived_with_{self._fuchsia_device_operation_obj}"
