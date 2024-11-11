# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.tracing.py."""

import os
import tempfile
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fidl.fuchsia_tracing_controller as f_tracingcontroller
import fuchsia_controller_py as fc
from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.fuchsia_controller import tracing as fc_tracing
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


class TracingFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.tracing.py."""

    def setUp(self) -> None:
        super().setUp()
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController
        )

        self.tracing_obj = fc_tracing.Tracing(
            device_name="fuchsia-emulator",
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "with_no_categories_and_no_buffer_size",
                },
            ),
            (
                {
                    "label": "with_categories_and_buffer_size",
                    "categories": ["category1", "category2"],
                    "buffer_size": 1024,
                },
            ),
            (
                {
                    "label": "when_session_already_initialized",
                    "session_initialized": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    def test_initialize(
        self,
        parameterized_dict: dict[str, Any],
        mock_tracingcontroller_initialize: mock.Mock,
    ) -> None:
        """Test for Tracing.initialize() method."""
        # Perform setup based on parameters.
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()

        # Check whether an `errors.TracingStateError` exception is raised when
        # calling `initialize()` on a session that is already initialized.
        if parameterized_dict.get("session_initialized"):
            with self.assertRaises(errors.TracingStateError):
                self.tracing_obj.initialize()
        else:
            self.tracing_obj.initialize(
                categories=parameterized_dict.get("categories"),
                buffer_size=parameterized_dict.get("buffer_size"),
            )
            mock_tracingcontroller_initialize.assert_called()

    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    def test_initialize_error(
        self, mock_tracingcontroller_initialize: mock.Mock
    ) -> None:
        """Test for Tracing.initialize() when the FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_tracingcontroller_initialize.side_effect = fc.ZxStatus(
            fc.ZxStatus.ZX_ERR_INVALID_ARGS
        )
        with self.assertRaises(errors.TracingError):
            self.tracing_obj.initialize()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                    "tracing_active": False,
                },
            ),
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                    "tracing_active": False,
                },
            ),
            (
                {
                    "label": "when_tracing_already_started",
                    "session_initialized": True,
                    "tracing_active": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    def test_start(
        self,
        parameterized_dict: dict[str, Any],
        mock_tracingcontroller_start: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.start() method."""
        # Perform setup based on parameters.
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()
        if parameterized_dict.get("tracing_active"):
            self.tracing_obj.start()

        # Check whether an `errors.TracingStateError` exception is raised when
        # state is not valid.
        if not parameterized_dict.get(
            "session_initialized"
        ) or parameterized_dict.get("tracing_active"):
            with self.assertRaises(errors.TracingStateError):
                self.tracing_obj.start()
        else:
            self.tracing_obj.start()
            mock_tracingcontroller_start.assert_called()

    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    def test_start_error(
        self, mock_tracingcontroller_start: mock.Mock, *unused_args: Any
    ) -> None:
        """Test for Tracing.start() when the FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        self.tracing_obj.initialize()

        mock_tracingcontroller_start.side_effect = fc.ZxStatus(
            fc.ZxStatus.ZX_ERR_INVALID_ARGS
        )
        with self.assertRaises(errors.TracingError):
            self.tracing_obj.start()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                    "tracing_active": False,
                },
            ),
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                    "tracing_active": False,
                },
            ),
            (
                {
                    "label": "when_tracing_already_started",
                    "session_initialized": True,
                    "tracing_active": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "stop_tracing",
        new_callable=mock.AsyncMock,
    )
    def test_stop(
        self,
        parameterized_dict: dict[str, Any],
        mock_tracingcontroller_stop: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.stop() method."""
        # Perform setup based on parameters.
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()
        if parameterized_dict.get("tracing_active"):
            self.tracing_obj.start()

        # Check whether an `errors.TracingStateError` exception is raised when
        # state is not valid.
        if not parameterized_dict.get(
            "session_initialized"
        ) or not parameterized_dict.get("tracing_active"):
            with self.assertRaises(errors.TracingStateError):
                self.tracing_obj.stop()
        else:
            self.tracing_obj.stop()
            mock_tracingcontroller_stop.assert_called()

    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "stop_tracing",
        new_callable=mock.AsyncMock,
    )
    def test_stop_error(
        self, mock_tracingcontroller_stop: mock.Mock, *unused_args: Any
    ) -> None:
        """Test for Tracing.stop() when the FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        self.tracing_obj.initialize()
        self.tracing_obj.start()

        mock_tracingcontroller_stop.side_effect = fc.ZxStatus(
            fc.ZxStatus.ZX_ERR_INVALID_ARGS
        )
        with self.assertRaises(errors.TracingError):
            self.tracing_obj.stop()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "with_no_download",
                    "session_initialized": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    def test_terminate(
        self,
        parameterized_dict: dict[str, Any],
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.terminate() method."""
        # Perform setup based on parameters.
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()

        # Check whether an `errors.TracingStateError` exception is raised when
        # state is not valid.
        self.tracing_obj.terminate()
        self.assertFalse(self.tracing_obj.is_active())
        # Check that no warning logs got printed.
        self.assertNoLogs()

    @parameterized.expand(
        [
            (
                {
                    "label": "with_unset_record_dropped",
                    "dropped": None,
                    "assert_warning": False,
                },
            ),
            (
                {
                    "label": "with_record_dropped",
                    "dropped": 10,
                    "assert_warning": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "stop_tracing",
        new_callable=mock.AsyncMock,
    )
    def test_stop_with_warning(
        self,
        parameterized_dict: dict[str, Any],
        mock_tracingcontroller_stop: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.stop() method with Warning."""
        # Perform setup based on parameters.
        records_dropped = parameterized_dict.get("dropped")
        mock_tracingcontroller_stop.return_value = mock.Mock(
            response=f_tracingcontroller.StopResult(
                provider_stats=[
                    f_tracingcontroller.ProviderStats(
                        name="virtual-console.cm",
                        pid=4566,
                        buffering_mode=1,
                        buffer_wrapped_count=0,
                        records_dropped=records_dropped,
                        percentage_durable_buffer_used=0.0,
                        non_durable_bytes_written=16,
                    )
                ]
            )
        )

        self.tracing_obj.initialize()
        self.tracing_obj.start()
        if parameterized_dict.get("assert_warning"):
            with self.assertLogs(level="WARNING") as lc:
                self.tracing_obj.stop()
                mock_tracingcontroller_stop.assert_called()
                self.assertIn(
                    f"{records_dropped} records were dropped for virtual-console.cm!",
                    lc.output[0],
                )
        else:
            with self.assertNoLogs(level="WARNING") as lc:
                self.tracing_obj.stop()

    @parameterized.expand(
        [
            (
                {
                    "label": "with_tracing_download_default_file_name",
                    "return_value": "samp_trace_data",
                },
            ),
            (
                {
                    "label": "with_tracing_download_given_file_name",
                    "trace_file": "trace.fxt",
                    "return_value": "samp_trace_data",
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(fc, "Channel")
    @mock.patch.object(fc, "Socket")
    def test_terminate_and_download(
        self,
        parameterized_dict: dict[str, Any],
        mock_fc_socket: mock.Mock,
        mock_fc_channel: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.terminate_and_download() method."""
        # Mock out the tracing Socket.
        return_value: str = parameterized_dict.get("return_value", "")
        mock_client_socket = mock.MagicMock()
        mock_client_socket.read.side_effect = [
            bytes(return_value, encoding="utf-8"),
            fc.ZxStatus(fc.ZxStatus.ZX_ERR_PEER_CLOSED),
        ]
        mock_fc_socket.create.return_value = (
            mock.MagicMock(),
            mock_client_socket,
        )

        mock_fc_channel.create.return_value = (
            mock.MagicMock(),
            mock.MagicMock(),
        )

        # Perform setup based on parameters.
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()

        with tempfile.TemporaryDirectory() as tmpdir:
            if not parameterized_dict.get("session_initialized"):
                with self.assertRaises(errors.TracingStateError):
                    self.tracing_obj.terminate_and_download(directory=tmpdir)
                return

            trace_file: str = parameterized_dict.get("trace_file", "")
            trace_path: str = self.tracing_obj.terminate_and_download(
                directory=tmpdir, trace_file=trace_file
            )
            self.assertFalse(self.tracing_obj.is_active())

            # Check the return value of the terminate method.
            if trace_file:
                self.assertEqual(trace_path, f"{tmpdir}/{trace_file}")
            else:
                self.assertRegex(trace_path, f"{tmpdir}/trace_.*.fxt")

            # Check the contents of the file.
            with open(trace_path, "r", encoding="utf-8") as file:
                data: str = file.read()
                self.assertEqual(data, return_value)

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                },
            ),
            (
                {
                    "label": "with_tracing_download",
                    "download_trace": True,
                    "trace_file": "trace.fxt",
                    "return_value": "samp_trace_data",
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        f_tracingcontroller.Provisioner.Client,
        "initialize_tracing",
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "start_tracing",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_tracingcontroller.Session.Client,
        "stop_tracing",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(fc, "Channel")
    @mock.patch.object(fc, "Socket")
    def test_trace_session(
        self,
        parameterized_dict: dict[str, Any],
        mock_fc_socket: mock.MagicMock,
        mock_fc_channel: mock.MagicMock,
        *unused_args: Any,
    ) -> None:
        """Test for Tracing.trace_session() method."""
        # Mock out the tracing Socket.
        return_value: str = parameterized_dict.get("return_value", "")
        mock_client_socket = mock.MagicMock()
        mock_client_socket.read.side_effect = [
            bytes(return_value, encoding="utf-8"),
            fc.ZxStatus(fc.ZxStatus.ZX_ERR_PEER_CLOSED),
        ]
        mock_fc_socket.create.return_value = (
            mock.MagicMock(),
            mock_client_socket,
        )

        mock_fc_channel.create.return_value = (
            mock.MagicMock(),
            mock.MagicMock(),
        )

        with tempfile.TemporaryDirectory() as tmpdir:
            trace_file: str = parameterized_dict.get("trace_file", "")
            download_trace: bool = parameterized_dict.get(
                "download_trace", False
            )

            if parameterized_dict.get("session_initialized"):
                self.tracing_obj.initialize()
            with self.tracing_obj.trace_session(
                download=download_trace, directory=tmpdir, trace_file=trace_file
            ):
                self.assertTrue(self.tracing_obj.is_active())
            self.assertFalse(self.tracing_obj.is_active())

            if download_trace:
                trace_path: str = os.path.join(tmpdir, trace_file)
                self.assertTrue(os.path.exists(trace_path))

                # Check the contents of the file.
                with open(trace_path, "r", encoding="utf-8") as file:
                    data: str = file.read()
                    self.assertEqual(data, return_value)


if __name__ == "__main__":
    unittest.main()
