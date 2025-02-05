# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia base test class."""

import enum
import logging
import os

from honeydew import errors
from honeydew.auxiliary_devices import power_switch_dmc
from honeydew.interfaces.auxiliary_devices import power_switch
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing import custom_types
from mobly import base_test, signals, test_runner
from mobly_controller import fuchsia_device as fuchsia_device_mobly_controller

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SnapshotOn(enum.StrEnum):
    """How often we need to collect the snapshot"""

    # Once per test case.
    TEARDOWN_TEST = "teardown_test"

    # Once per test case on failure only.
    TEARDOWN_TEST_ON_FAIL = "teardown_test_on_fail"

    # Once per test class.
    TEARDOWN_CLASS = "teardown_class"

    # Once per test class on failure only.
    TEARDOWN_CLASS_ON_FAIL = "teardown_class_on_fail"

    # Do not collect snapshot
    NEVER = "never"


class TracingOn(enum.StrEnum):
    """Tracing behavior for tests.

    This user param does not support any tests that reboot the device.
    """

    # Once per test case.
    TEARDOWN_TEST = "teardown_test"

    # Once per test case on failure only.
    TEARDOWN_TEST_ON_FAIL = "teardown_test_on_fail"

    # Once per test class.
    TEARDOWN_CLASS = "teardown_class"

    # Once per test class on failure only.
    TEARDOWN_CLASS_ON_FAIL = "teardown_class_on_fail"

    # Do not collect
    NEVER = "never"


class FuchsiaBaseTest(base_test.BaseTestClass):
    """Fuchsia base test class.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
        snapshot_on: `snapshot_on` test param value converted into SnapshotOn
            Enum.
        tracing_on: `tracing_on` test param value converted into TracingOn

    Required Mobly Test Params:
        snapshot_on (str): One of "teardown_class", "teardown_class_on_fail",
            "teardown_test", "on_fail".
            Default value is "teardown_class_on_fail".
        tracing_on (str): One of "teardown_class", "teardown_class_on_fail",
            "teardown_test", "on_fail", "never".
        # TODO(b/378563090): Switch the default to `teardown_class_on_fail` after
        # refactoring lacewing tests
            Default value is "never".
    """

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Reads user params passed to the test
            * Instantiates all fuchsia devices into self.fuchsia_devices
            * Instantiates and starts tracing if specified in the user params
        """
        self._any_test_failed: bool = False
        self._process_metric_user_params()

        self.fuchsia_devices: list[
            fuchsia_device.FuchsiaDevice
        ] = self.register_controller(fuchsia_device_mobly_controller)

        if (
            self.tracing_on == TracingOn.TEARDOWN_CLASS
            or self.tracing_on == TracingOn.TEARDOWN_CLASS_ON_FAIL
        ):
            for device in self.fuchsia_devices:
                device.tracing.initialize()
                device.tracing.start()

    def setup_test(self) -> None:
        """setup_test is called once before running each test.

        It does the following things:
            * Stores the current test case path into self.test_case_path
            * Logs a info message onto device that test case has started.
            * Instantiates and starts tracing if specified in the user params
        """
        self._devices_not_healthy: bool = False

        self.test_case_path: str = (
            f"{self.log_path}/{self.current_test_info.name}"
        )
        os.mkdir(self.test_case_path)
        self._log_message_to_devices(
            message=f"Started executing '{self.current_test_info.name}' "
            f"Lacewing test case...",
            level=custom_types.LEVEL.INFO,
        )
        for device in self.fuchsia_devices:
            if (
                not device.tracing.is_active()
                and not device.tracing.is_session_initialized()
            ):
                if (
                    self.tracing_on == TracingOn.TEARDOWN_TEST
                    or self.tracing_on == TracingOn.TEARDOWN_TEST_ON_FAIL
                ):
                    device.tracing.initialize()
                    device.tracing.start()

    def teardown_test(self) -> None:
        """teardown_test is called once after running each test.

        It does the following things:
            * Takes snapshot of all the fuchsia devices and stores it under
              test case directory if `snapshot_on` test param is set to
              "teardown_test"
            * Logs a info message onto device that test case has ended.
        """
        self._health_check_and_recover()

        if self.snapshot_on == SnapshotOn.TEARDOWN_TEST:
            self._collect_snapshot(directory=self.test_case_path)

        for device in self.fuchsia_devices:
            if (
                device.tracing.is_active()
                and device.tracing.is_session_initialized()
            ):
                if self.tracing_on == TracingOn.TEARDOWN_TEST:
                    device.tracing.stop()
                    device.tracing.terminate_and_download(
                        directory=self.test_case_path
                    )

        self._log_message_to_devices(
            message=f"Finished executing '{self.current_test_info.name}' "
            f"Lacewing test case...",
            level=custom_types.LEVEL.INFO,
        )
        if len(os.listdir(self.test_case_path)) == 0:
            os.rmdir(self.test_case_path)

        if self._devices_not_healthy:
            message: str = (
                "One or more FuchsiaDevice's health check failed in "
                "teardown_test. So failing the test case..."
            )
            _LOGGER.warning(message)
            raise signals.TestFailure(message)

    def teardown_class(self) -> None:
        """teardown_class is called once after running all tests.

        It does the following things:
            * Takes snapshot of all the fuchsia devices and stores it under
              "<log_path>/teardown_class<_on_fail>" directory if `snapshot_on`
              test param is set to "teardown_class" or "teardown_class_on_fail".
            * Stops, terminates and downloads the trace data for all devices and stores
              it under "<log_path>/teardown_class<_on_fail>" directory if `tracing_on`
              test param is set to "teardown_class" or "teardown_class_on_fail".
        """
        self._teardown_class_artifacts: str = f"{self.log_path}/teardown_class"

        for device in self.fuchsia_devices:
            if (
                device.tracing.is_active()
                and device.tracing.is_session_initialized()
            ):
                if self.tracing_on == TracingOn.TEARDOWN_CLASS:
                    device.tracing.stop()
                    device.tracing.terminate_and_download(
                        directory=self._teardown_class_artifacts
                    )
                elif (
                    self.tracing_on == TracingOn.TEARDOWN_CLASS_ON_FAIL
                    and self._any_test_failed
                ):
                    device.tracing.stop()
                    device.tracing.terminate_and_download(
                        directory=self._teardown_class_artifacts
                    )

        if self.snapshot_on == SnapshotOn.TEARDOWN_CLASS:
            self._teardown_class_artifacts = f"{self.log_path}/teardown_class"
            self._collect_snapshot(directory=self._teardown_class_artifacts)
        elif (
            self.snapshot_on == SnapshotOn.TEARDOWN_CLASS_ON_FAIL
            and self._any_test_failed
        ):
            self._teardown_class_artifacts = (
                f"{self.log_path}/teardown_class_on_fail"
            )
            self._collect_snapshot(directory=self._teardown_class_artifacts)

    def on_fail(self, _) -> None:  # type: ignore[no-untyped-def]
        """on_fail is called once when a test case fails.

        It does the following things:
            * Takes snapshot of all the fuchsia devices and stores it under
              test case directory if `snapshot_on` test param is set to
              "on_fail"
        """
        self._any_test_failed = True
        if self.snapshot_on == SnapshotOn.TEARDOWN_TEST_ON_FAIL:
            self._collect_snapshot(directory=self.test_case_path)

        for device in self.fuchsia_devices:
            if (
                device.tracing.is_active()
                and device.tracing.is_session_initialized()
            ):
                if self.tracing_on == TracingOn.TEARDOWN_TEST_ON_FAIL:
                    for device in self.fuchsia_devices:
                        device.tracing.stop()
                        device.tracing.terminate_and_download(
                            directory=self.test_case_path
                        )

    def _collect_snapshot(self, directory: str) -> None:
        """Collects snapshots for all the FuchsiaDevice objects and stores them
        in the directory specified.

        Args:
            directory: Absolute path on the host where snapshot file need to be
                saved.
        """
        if not hasattr(self, "fuchsia_devices"):
            return

        _LOGGER.info(
            "Collecting snapshots of all the FuchsiaDevice objects in '%s'...",
            self.snapshot_on.value,
        )
        for fx_device in self.fuchsia_devices:
            try:
                fx_device.snapshot(directory=directory)
            except Exception as err:  # pylint: disable=broad-except
                _LOGGER.exception(
                    "Unable to take snapshot of %s. Failed with error: %s",
                    fx_device.device_name,
                    err,
                )

    def _get_controller_configs(
        self, controller_type: str
    ) -> list[dict[str, object]]:
        """Return testbed config associated with a specific Mobly Controller.

        Args:
            controller_type: Controller type that is included in mobly testbed.
                Ex: 'FuchsiaDevice', 'AndroidDevice' etc

        Returns:
            Config specified in the testbed file that is associated with
            controller type provided.

        Example:
            ```
            TestBeds:
            - Name: Testbed-One-X64
                Controllers:
                  FuchsiaDevice:
                    - name: fuchsia-54b2-038b-6e90
                      transport: default
            ```

            For above specified testbed file, calling
            ```
            get_controller_configs(controller_type="FuchsiaDevice")
            ```
            will return
            ```
            [
                {
                    'name': 'fuchsia-54b2-038b-6e90',
                    'transport': 'default'
                }
            ]
            ```
        """
        for (
            controller_name,
            controller_configs,
        ) in self.controller_configs.items():
            if controller_name == controller_type:
                return controller_configs
        return []

    def _get_device_config(
        self, controller_type: str, identifier_key: str, identifier_value: str
    ) -> dict[str, object]:
        """Return testbed config associated with a specific device of a
        particular mobly controller type.

        Args:
            controller_type: Controller type that is included in mobly testbed.
                Ex: 'FuchsiaDevice', 'AndroidDevice' etc
            identifier_key: Key to identify the specific device.
                Ex: 'name', 'nodename' etc
            identifier_value: Value to match from list of devices.
                Ex: 'fuchsia-emulator' etc

        Returns:
            Config specified in the testbed file that is associated with
            controller type provided.

        Example:
            ```
            TestBeds:
            - Name: Testbed-One-X64
                Controllers:
                  FuchsiaDevice:
                    - name: fuchsia-54b2-038b-6e90
                      transport: default
            ```

            For above specified testbed file, calling
            ```
                get_testbed_config(
                    controller_type="FuchsiaDevice",
                    identifier_key="name",
                    identifier_value="fuchsia-emulator")
            ```
            will return
            ```
            {
                'name': 'fuchsia-54b2-038b-6e90',
                'transport': 'default'
            }
            ```
        """
        for controller_config in self._get_controller_configs(controller_type):
            if controller_config[identifier_key] == identifier_value:
                _LOGGER.info(
                    "Device configuration associated with %s is %s",
                    identifier_value,
                    controller_config,
                )
                return controller_config
        return {}

    def _health_check_and_recover(self) -> None:
        """Ensure all FuchsiaDevice objects are healthy and if unhealthy perform
        a power_cycle in an attempt to recover.

        If health check failed for any device then fail the test case even if we
        are able to recover the device successfully.

        If the recovery fails, then abort the test class.
        """
        _LOGGER.info(
            "Performing health checks on all the FuchsiaDevice objects..."
        )

        for fx_device in self.fuchsia_devices:
            try:
                fx_device.health_check()
            except errors.HealthCheckError as err:
                self._devices_not_healthy = True
                _LOGGER.warning(
                    "Health check on %s failed with error '%s', will try to recover the device",
                    fx_device.device_name,
                    err,
                )
                self._recover_device(fx_device)

        _LOGGER.info(
            "Successfully performed health checks and/or recoveries on all the "
            "FuchsiaDevice objects..."
        )

    def _recover_device(self, fx_device: fuchsia_device.FuchsiaDevice) -> None:
        """Try to recover the fuchsia device by power cycling it if the test has
        access to DMC.

        Args:
            fx_device: FuchsiaDevice object
        """
        try:
            dmc_power_switch: power_switch_dmc.PowerSwitchDmc = (
                power_switch_dmc.PowerSwitchDmc(
                    device_name=fx_device.device_name
                )
            )
            fx_device.power_cycle(power_switch=dmc_power_switch, outlet=None)
        except power_switch_dmc.PowerSwitchDmcError as err:
            _LOGGER.warning(
                "Unable to power cycle %s as test does not have access to DMC. "
                "Aborting the test class...",
                fx_device.device_name,
            )
            raise signals.TestAbortClass(
                f"{fx_device.device_name} is unhealthy and unable to recover it"
            ) from err
        except power_switch.PowerSwitchError as err:
            _LOGGER.warning(
                "Power cycling %s failed with error '%s'. "
                "Aborting the test class...",
                fx_device.device_name,
                err,
            )
            raise signals.TestAbortClass(
                f"{fx_device.device_name} is unhealthy and failed to recover it"
            ) from err

    def _log_message_to_devices(
        self, message: str, level: custom_types.LEVEL
    ) -> None:
        """Log message in all the Fuchsia devices.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """
        for fx_device in self.fuchsia_devices:
            try:
                fx_device.log_message_to_device(message, level)
            except Exception as err:  # pylint: disable=broad-except
                _LOGGER.exception(
                    "Unable to log message '%s' on '%s'. Failed with error: %s",
                    message,
                    fx_device.device_name,
                    err,
                )

    def _process_metric_user_params(self) -> None:
        """Reads, processes and stores the metric collection params used by this module.

        At the moment we collect snapshots and traces.

        Raises:
            TestAbortClass: When user_params provided are invalid.
        """
        _LOGGER.info(
            "user_params associated with the test: %s", self.user_params
        )

        snapshot_on: str = self.user_params.get(
            "snapshot_on", SnapshotOn.TEARDOWN_CLASS_ON_FAIL.value
        ).lower()
        tracing_on: str = self.user_params.get(
            "tracing_on", SnapshotOn.NEVER.value
        ).lower()

        try:
            self.snapshot_on: SnapshotOn = SnapshotOn(snapshot_on)
            self.tracing_on: TracingOn = TracingOn(tracing_on)
        except ValueError as e:
            raise signals.TestAbortClass("invalid metric user_param") from e


if __name__ == "__main__":
    test_runner.main()
