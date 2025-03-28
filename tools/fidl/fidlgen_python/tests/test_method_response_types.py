# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

import fidl_test_python_protocol as test_python_protocol
from fidl import FrameworkError


class MethodResponseTypesTestsuite(unittest.TestCase):
    """
    These tests merely check that the result and response types for each kind of FIDL
    protocol method are generated (or not) in the expected form. Other test suites
    verify the behavior of the methods themselves.
    """

    def test_one_way(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodOneWayResponse",
            )
        )

    def test_empty_response(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodEmptyResponseResponse",
            )
        )

    def test_empty_response_with_error(self) -> None:
        test_python_protocol.ClosedProtocolStrictMethodEmptyResponseWithErrorResult.response_variant(
            None
        )
        test_python_protocol.ClosedProtocolStrictMethodEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_non_empty_response(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodNonEmptyResponseResult",
            )
        )
        test_python_protocol.ClosedProtocolStrictMethodNonEmptyResponseResponse(
            b=True
        )

    def test_non_empty_response_with_error(self) -> None:
        test_python_protocol.ClosedProtocolStrictMethodNonEmptyResponseWithErrorResult.response_variant(
            test_python_protocol.ClosedProtocolStrictMethodNonEmptyResponseWithErrorResponse(
                b=True
            )
        )
        test_python_protocol.ClosedProtocolStrictMethodNonEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_with_args_one_way(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodWithArgsOneWayResponse",
            )
        )

    def test_with_args_empty_response(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodWithArgsEmptyResponseResponse",
            )
        )

    def test_with_args_empty_response_with_error(self) -> None:
        test_python_protocol.ClosedProtocolStrictMethodWithArgsEmptyResponseWithErrorResult.response_variant(
            None
        )
        test_python_protocol.ClosedProtocolStrictMethodWithArgsEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_with_args_non_empty_response(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolStrictMethodWithArgsNonEmptyResponseResult",
            )
        )
        test_python_protocol.ClosedProtocolStrictMethodWithArgsNonEmptyResponseResponse(
            b=True
        )

    def test_with_args_non_empty_response_with_error(self) -> None:
        test_python_protocol.ClosedProtocolStrictMethodWithArgsNonEmptyResponseWithErrorResult.response_variant(
            test_python_protocol.ClosedProtocolStrictMethodWithArgsNonEmptyResponseWithErrorResponse(
                b=True
            )
        )
        test_python_protocol.ClosedProtocolStrictMethodWithArgsNonEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_empty_event(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "ClosedProtocolOnStrictEmptyEventRequest",
            )
        )

    def test_non_empty_event(self) -> None:
        event = test_python_protocol.ClosedProtocolOnStrictNonEmptyEventRequest(
            b=True
        )

    def test_flexible_one_way(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "AjarProtocolFlexibleMethodOneWayResponse",
            )
        )

    def test_flexible_with_args_one_way(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "AjarProtocolFlexibleWithArgsMethodOneWayResponse",
            )
        )

    def test_flexible_empty_event(self) -> None:
        self.assertFalse(
            hasattr(
                test_python_protocol,
                "AjarProtocolOnFlexibleEmptyEventRequest",
            )
        )

    def test_flexible_non_empty_event(self) -> None:
        test_python_protocol.AjarProtocolOnFlexibleNonEmptyEventRequest(b=True)

    def test_flexible_empty_response(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodEmptyResponseResult.response_variant(
            None
        )
        test_python_protocol.OpenProtocolFlexibleMethodEmptyResponseResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )

    def test_flexible_empty_response_with_error(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodEmptyResponseWithErrorResult.response_variant(
            None
        )
        test_python_protocol.OpenProtocolFlexibleMethodEmptyResponseWithErrorResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )
        test_python_protocol.OpenProtocolFlexibleMethodEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_flexible_non_empty_response(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseResult.response_variant(
            test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseResponse(
                b=True
            )
        )
        test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )

    def test_flexible_non_empty_response_with_error(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseWithErrorResult.response_variant(
            test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseWithErrorResponse(
                b=True
            )
        )
        test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseWithErrorResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )
        test_python_protocol.OpenProtocolFlexibleMethodNonEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_flexible_with_args_empty_response(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsEmptyResponseResult.response_variant(
            None
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsEmptyResponseResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )

    def test_flexible_with_args_empty_response_with_error(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsEmptyResponseWithErrorResult.response_variant(
            None
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsEmptyResponseWithErrorResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsEmptyResponseWithErrorResult.err_variant(
            2
        )

    def test_flexible_with_args_non_empty_response(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseResult.response_variant(
            test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseResponse(
                b=True
            )
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )

    def test_flexible_with_args_non_empty_response_with_error(self) -> None:
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseWithErrorResult.response_variant(
            test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseWithErrorResponse(
                b=True
            )
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseWithErrorResult.framework_err_variant(
            FrameworkError.UNKNOWN_METHOD
        )
        test_python_protocol.OpenProtocolFlexibleMethodWithArgsNonEmptyResponseWithErrorResult.err_variant(
            2
        )
