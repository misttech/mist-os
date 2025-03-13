# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Lacewing Test Case Reviver."""

import dataclasses
import enum
import importlib
import logging
from collections.abc import Callable
from typing import Any

from fuchsia_base_test import fuchsia_base_test
from honeydew.auxiliary_devices.power_switch import power_switch

_LOGGER: logging.Logger = logging.getLogger(__name__)


_DMC_MODULE: str = (
    "honeydew.auxiliary_devices.power_switch.power_switch_using_dmc"
)
_DMC_CLASS: str = "PowerSwitchUsingDmc"


# pylint: disable=protected-access


class FuchsiaDeviceOperation(enum.StrEnum):
    """Operation that need to be performed on Fuchsia Device."""

    NONE = "none"

    SOFT_REBOOT = "soft-reboot"

    HARD_REBOOT = "hard-reboot"

    POWER_CYCLE = "power-cycle"

    IDLE_SUSPEND_TIMER_RESUME = "idle-suspend-timer-resume"

    IDLE_SUSPEND_BUTTON_PRESS_RESUME = "idle-suspend-button-press-resume"


class TestMethodExecutionFrequency(enum.StrEnum):
    """How often the test case method need to be executed in the revived test case."""

    # This will result in: Run_Test -> Run_Device_Operation -> Run_Test
    PRE_AND_POST = "Pre-and-Post"

    # This will result in: Run_Test -> Run_Device_Operation
    PRE_ONLY = "Pre-Only"

    # This will result in: Run_Device_Operation -> Run_Test
    POST_ONLY = "Post-Only"


@dataclasses.dataclass(frozen=True)
class _TestArgTuple:
    test_case_method: str
    fuchsia_device_operation: FuchsiaDeviceOperation
    test_method_execution_frequency: TestMethodExecutionFrequency

    def __str__(self) -> str:
        return (
            f"TestCaseMethod:{self.test_case_method}, "
            f"FuchsiaDeviceOperation:{self.fuchsia_device_operation}, "
            f"TestMethodExecutionFrequency:{self.test_method_execution_frequency}"
        )


def tag_test(
    tag_name: str = "revive_test_case",
    fuchsia_device_operation: FuchsiaDeviceOperation | None = None,
    test_method_execution_frequency: TestMethodExecutionFrequency | None = None,
) -> Callable[..., Any]:
    """Decorator that can be used to tag a test with a label"""

    def tags_decorator(func: Callable[..., Any]) -> Callable[..., Any]:
        func._tag = tag_name  # type: ignore[attr-defined]
        if fuchsia_device_operation is not None:
            func._fuchsia_device_operation = FuchsiaDeviceOperation(  # type: ignore[attr-defined]
                fuchsia_device_operation
            )
        if test_method_execution_frequency is not None:
            func._test_method_execution_frequency = (  # type: ignore[attr-defined]
                TestMethodExecutionFrequency(test_method_execution_frequency)
            )
        return func

    return tags_decorator


class TestCaseRevive(fuchsia_base_test.FuchsiaBaseTest):
    """Test case revive is a lacewing test class that takes any Lacewing test
    case and modifies it to run in below sequence:

    1. Run the test case (optional, if specified by user)
    2. Perform an operation requested by user (from list of supported operations)
    3. Rerun the test case (optional, if specified by user)
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

        _LOGGER.info(
            "[TestCaseRevive] - test_case_revive setting is enabled in "
            "user_params. So testing in revive mode...",
        )

        self._read_and_validate_user_params()

        revived_test_cases: list[str] = self._get_list_of_revived_test_cases()

        test_arg_tuple_list: list[
            _TestArgTuple
        ] = self._generate_test_args_tuple_list(revived_test_cases)

        self.generate_tests(
            test_logic=self._logic_for_test_case_revive,
            name_func=self._revived_test_case_name_func,
            arg_sets=[
                dataclasses.astuple(test_arg_tuple)
                for test_arg_tuple in test_arg_tuple_list
            ],
        )

    def _perform_op(
        self, fuchsia_device_operation: FuchsiaDeviceOperation
    ) -> None:
        """Perform user specified operation"""

        for fuchsia_device in self.fuchsia_devices:
            if (
                fuchsia_device_operation
                == FuchsiaDeviceOperation.IDLE_SUSPEND_TIMER_RESUME
            ):
                fuchsia_device.system_power_state_controller.idle_suspend_timer_based_resume(
                    duration=self.user_params["resume_timer_duration_sec"],
                    verify_duration=self.user_params.get(
                        "verify_duration", True
                    ),
                )
            elif fuchsia_device_operation == FuchsiaDeviceOperation.SOFT_REBOOT:
                fuchsia_device.reboot()
            elif fuchsia_device_operation in [
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
            elif (
                fuchsia_device_operation
                == FuchsiaDeviceOperation.IDLE_SUSPEND_BUTTON_PRESS_RESUME
            ):
                raise ValueError(
                    f"'{fuchsia_device_operation}' operation is not supported by 'TestCaseRevive'"
                )

    def _logic_for_test_case_revive(
        self,
        test_case: str,
        fuchsia_device_operation: FuchsiaDeviceOperation,
        test_method_execution_frequency: TestMethodExecutionFrequency,
    ) -> None:
        """TestCaseRevive logic"""

        sequence: str
        if (
            test_method_execution_frequency
            == TestMethodExecutionFrequency.PRE_AND_POST
        ):
            sequence = (
                f"{test_case} -> {fuchsia_device_operation} -> {test_case}"
            )
        elif (
            test_method_execution_frequency
            == TestMethodExecutionFrequency.PRE_ONLY
        ):
            sequence = f"{test_case} -> {fuchsia_device_operation}"
        else:  # TestMethodExecutionFrequency.POST_ONLY
            sequence = f"{fuchsia_device_operation} -> {test_case}"
        _LOGGER.info("[TestCaseRevive] - Revived test logic: %s", sequence)

        if test_method_execution_frequency in [
            TestMethodExecutionFrequency.PRE_AND_POST,
            TestMethodExecutionFrequency.PRE_ONLY,
        ]:
            _LOGGER.info(
                "[TestCaseRevive] - Running the %s before performing %s operation...",
                test_case,
                fuchsia_device_operation,
            )
            getattr(self, test_case)()

        _LOGGER.info(
            "[TestCaseRevive] - Performing %s operation on all Fuchsia devices "
            "that are part of the testbed...",
            fuchsia_device_operation,
        )
        self._perform_op(fuchsia_device_operation=fuchsia_device_operation)

        if test_method_execution_frequency in [
            TestMethodExecutionFrequency.PRE_AND_POST,
            TestMethodExecutionFrequency.POST_ONLY,
        ]:
            _LOGGER.info(
                "[TestCaseRevive] - Running the %s after performing %s operation...",
                test_case,
                fuchsia_device_operation,
            )
            getattr(self, test_case)()

    def _revived_test_case_name_func(
        self,
        test_case: str,
        fuchsia_device_operation: FuchsiaDeviceOperation,
        _: TestMethodExecutionFrequency,
    ) -> str:
        """Revived test case name function"""
        test_case = test_case.lstrip("_")
        return f"{test_case}_revived_with_{fuchsia_device_operation}"

    def _read_and_validate_user_params(self) -> None:
        """Read the user params associated with this test"""
        self._fuchsia_device_operation: str = self.user_params.get(
            "fuchsia_device_operation", FuchsiaDeviceOperation.NONE
        )
        try:
            self._fuchsia_device_operation_obj: FuchsiaDeviceOperation = (
                FuchsiaDeviceOperation(self._fuchsia_device_operation.lower())
            )
        except ValueError as err:
            raise ValueError(
                f"'{self._fuchsia_device_operation}' operation is not "
                f"supported by 'TestCaseRevive'"
            ) from err

        if (
            self._fuchsia_device_operation_obj
            == FuchsiaDeviceOperation.IDLE_SUSPEND_TIMER_RESUME
        ):
            if not isinstance(
                self.user_params.get("resume_timer_duration_sec"), int
            ):
                raise ValueError(
                    f"FuchsiaDeviceOperation: '{self._fuchsia_device_operation}' "
                    f"requires user to also pass 'resume_timer_duration_sec' key "
                    f"with int value in test prams"
                )

        self._test_method_execution_frequency: str = self.user_params.get(
            "test_method_execution_frequency",
            TestMethodExecutionFrequency.PRE_AND_POST,
        )
        try:
            self._test_method_execution_frequency_obj: (
                TestMethodExecutionFrequency
            ) = TestMethodExecutionFrequency(
                self._test_method_execution_frequency
            )
        except ValueError as err:
            raise ValueError(
                f"'{self._test_method_execution_frequency}' is not valid"
            ) from err

    def _get_list_of_revived_test_cases(self) -> list[str]:
        """Return the list of test cases that need to be revived."""
        test_cases: list[str] = [
            attribute
            for attribute in dir(self)
            if callable(getattr(self, attribute))
            and attribute.startswith(("test_", "_test_")) is True
        ]
        _LOGGER.info(
            "[TestCaseRevive] - List of all the test cases in this test "
            "class: %s",
            test_cases,
        )

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

        return revived_test_cases

    def _generate_test_args_tuple_list(
        self, revived_test_cases: list[str]
    ) -> list[_TestArgTuple]:
        """Generate the list of duple data structure that need to be passed to
        Mobly's generate_tests() method."""

        test_arg_tuple_list: list[_TestArgTuple] = []
        for revived_test_case in revived_test_cases:
            fuchsia_device_operation: FuchsiaDeviceOperation = (
                self._fuchsia_device_operation_obj
            )
            if "_fuchsia_device_operation" in dir(
                getattr(self, revived_test_case)
            ):
                fuchsia_device_operation = getattr(
                    self, revived_test_case
                )._fuchsia_device_operation

            test_method_execution_frequency: TestMethodExecutionFrequency = (
                self._test_method_execution_frequency_obj
            )
            if "_test_method_execution_frequency" in dir(
                getattr(self, revived_test_case)
            ):
                test_method_execution_frequency = getattr(
                    self, revived_test_case
                )._test_method_execution_frequency

            test_arg_tuple: _TestArgTuple = _TestArgTuple(
                test_case_method=revived_test_case,
                fuchsia_device_operation=fuchsia_device_operation,
                test_method_execution_frequency=test_method_execution_frequency,
            )

            test_arg_tuple_list.append(test_arg_tuple)

        _LOGGER.debug(
            "[TestCaseRevive] - Test arg list that will be passed to Mobly's "
            "generate_test method to generate the revived test cases is: %s",
            test_arg_tuple_list,
        )

        return test_arg_tuple_list
