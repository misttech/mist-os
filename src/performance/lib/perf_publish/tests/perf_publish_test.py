#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for the perf metric publishing code."""

from typing import Any, Callable, Iterable
import json
import os
import random
import tempfile
import unittest
import unittest.mock as mock

import perf_publish.publish as publish

# Test data

_EMPTY_FUCHSIA_PERF = json.dumps([], indent=4)

_EXPECTED_METRICS = """fuchsia.my.benchmark: metric_1
fuchsia.my.benchmark: metric_2
# comments are allowed and ignored
fuchsia.my.benchmark: metric_3
fuchsia.my.benchmark: metric_4 [optional]
"""

_EXPECTED_METRICS_NO_SUMMARIZE = (
    f"""[no-summarize-metrics]\n{_EXPECTED_METRICS}"""
)

_EXPECTED_METRICS_FILE = "expected_metrics.txt"
_EXPECTED_METRICS_NO_SUMMARIZE_FILE = "expected_metrics_no_summarize.txt"
_EMPTY_EXPECTED_METRICS_FILE = "empty_metrics.txt"

_TEST_FUCHSIA_PERF = json.dumps(
    [
        {
            "label": "metric_1",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [1, 2, 3, 4],
        },
        {
            "label": "metric_2",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [5, 6, 7, 8],
        },
        {
            "label": "metric_3",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [9, 10, 11, 12],
        },
    ],
    indent=4,
)

_INVALID_SUITE_FUCHSIA_PERF = json.dumps(
    [
        {
            "label": "metric_1",
            "test_suite": "invalid_test_suite_name",
            "unit": "ms",
            "values": [1, 2, 3, 4],
        },
        {
            "label": "metric_2",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [5, 6, 7, 8],
        },
        {
            "label": "metric_3",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [9, 10, 11, 12],
        },
    ],
    indent=4,
)

_MISMATCH_METRICS_FUCHSIA_PERF = json.dumps(
    [
        {
            "label": "metric_1",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [1, 2, 3, 4],
        },
        {
            "label": "unexpected",
            "test_suite": "fuchsia.my.benchmark",
            "unit": "ms",
            "values": [1, 2, 3, 4],
        },
    ],
    indent=4,
)


class Validator:
    """Class for custom call arg validation."""

    def __init__(self, validator: Callable[[str], bool]):
        self.validator = validator

    def __eq__(self, comparand: Any) -> bool:
        return self.validator(comparand)


_CATAPULT_CONVERTER_VALIDATOR = Validator(
    lambda x: x.endswith("catapult_converter")
)


class CatapultConverterTest(unittest.TestCase):
    """Catapult converter metric publishing tests"""

    def setUp(self) -> None:
        self._temp_dir = tempfile.TemporaryDirectory()
        self._expected_metrics_txt: str = self._init_file(
            _EXPECTED_METRICS_FILE, _EXPECTED_METRICS
        )
        self._expected_metrics_no_summarize_txt: str = self._init_file(
            _EXPECTED_METRICS_NO_SUMMARIZE_FILE, _EXPECTED_METRICS_NO_SUMMARIZE
        )
        self._empty_expected_metrics: str = self._init_file(
            _EMPTY_EXPECTED_METRICS_FILE, ""
        )
        self._empty_fuchsia_perf_json: str = self._init_file(
            "empty.fuchsiaperf.json", _EMPTY_FUCHSIA_PERF
        )
        self._test_fuchsia_perf_json: str = self._init_file(
            "test.fuchsiaperf.json", _TEST_FUCHSIA_PERF
        )
        self._invalid_suite_fuchsia_perf_json: str = self._init_file(
            "invalid_suite.fuchsiaperf.json", _INVALID_SUITE_FUCHSIA_PERF
        )
        self._mismatch_metrics_fuchsia_perf_json: str = self._init_file(
            "mismatch_metrics.fuchsiaperf.json", _MISMATCH_METRICS_FUCHSIA_PERF
        )
        self._expected_input_path: str = os.path.join(
            self._temp_dir.name, "results.fuchsiaperf.json"
        )
        self._expected_output_path: str = os.path.join(
            self._temp_dir.name, "results.catapult_json"
        )
        self._expected_local_output_path: str = os.path.join(
            self._temp_dir.name, "results.catapult_json_disabled"
        )

    def tearDown(self) -> None:
        self._temp_dir.cleanup()

    def make_catapult_converter_for_test(
        self,
        fuchsia_perf_file_paths: Iterable[str | os.PathLike[str]],
        expected_metric_names_filename: str,
        env: dict[str, str],
        subprocess_check_call: Any,
    ) -> publish.CatapultConverter:
        """Create a CatapultConverter for testing purposes.

        Most test cases here should use this.  It has a non-optional "env"
        argument to ensure that the test does not accidentally depend on
        the environment (to avoid failing when run on a release branch in
        Infra; see b/328272533).
        """
        return publish.CatapultConverter.from_env(
            fuchsia_perf_file_paths,
            expected_metric_names_filename,
            env=env,
            runtime_deps_dir=self._temp_dir.name,
            current_time=12345,
            subprocess_check_call=subprocess_check_call,
        )

    def test_run_converter_local(self) -> None:
        """Test case that ensures we correctly run the Converter with local args"""
        subprocess_check_call: mock.Mock = mock.Mock()
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._empty_fuchsia_perf_json],
                _EMPTY_EXPECTED_METRICS_FILE,
                env={
                    publish.ENV_RELEASE_VERSION: "1",
                },
                subprocess_check_call=subprocess_check_call,
            )
        )

        # Files are moved to a `fuchsiaperf_full.json` file given that they are
        # summarized into a `fuchsiaperf.json` file.
        self.assertFalse(os.path.isfile(self._empty_fuchsia_perf_json))
        self.assertTrue(
            os.path.isfile(
                self._empty_fuchsia_perf_json.replace(
                    "fuchsiaperf.json", "fuchsiaperf_full.json"
                )
            )
        )

        converter.run()

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_local_output_path,
                "--execution-timestamp-ms",
                "12345000",
                "--masters",
                "local-master",
                "--log-url",
                "http://ci.example.com/build/300",
                "--bots",
                "local-bot",
                "--product-versions",
                "1",
            ]
        )

    def test_converter_summarizes_input(self) -> None:
        """Test case that ensures we correctly run the Converter with local args"""
        subprocess_check_call: mock.Mock = mock.Mock()
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._test_fuchsia_perf_json],
                self._expected_metrics_txt,
                env={
                    publish.ENV_RELEASE_VERSION: "1",
                },
                subprocess_check_call=subprocess_check_call,
            )
        )

        converter.run()

        self.assertTrue(os.path.isfile(self._expected_input_path))

        with open(self._expected_input_path, "r") as f:
            input_data = json.load(f)
            self.assertEqual(
                input_data,
                [
                    {
                        "label": "metric_1",
                        "test_suite": "fuchsia.my.benchmark",
                        "unit": "ms",
                        "values": [3],
                    },
                    {
                        "label": "metric_2",
                        "test_suite": "fuchsia.my.benchmark",
                        "unit": "ms",
                        "values": [7],
                    },
                    {
                        "label": "metric_3",
                        "test_suite": "fuchsia.my.benchmark",
                        "unit": "ms",
                        "values": [11],
                    },
                ],
            )

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_local_output_path,
                "--execution-timestamp-ms",
                "12345000",
                "--masters",
                "local-master",
                "--log-url",
                "http://ci.example.com/build/300",
                "--bots",
                "local-bot",
                "--product-versions",
                "1",
            ]
        )

    def test_run_converter_ci(self) -> None:
        """
        Test case that ensures that we correctly run the Converter with CI args
        """
        subprocess_check_call: mock.Mock = mock.Mock()
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._empty_fuchsia_perf_json],
                _EMPTY_EXPECTED_METRICS_FILE,
                env={
                    publish.ENV_CATAPULT_DASHBOARD_MASTER: "the-master",
                    publish.ENV_CATAPULT_DASHBOARD_BOT: "the-bot",
                    publish.ENV_BUILDBUCKET_ID: "bucket-123",
                    publish.ENV_BUILD_CREATE_TIME: "98765",
                    publish.ENV_RELEASE_VERSION: "2",
                },
                subprocess_check_call=subprocess_check_call,
            )
        )

        converter.run()

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_output_path,
                "--execution-timestamp-ms",
                "98765",
                "--masters",
                "the-master",
                "--log-url",
                "https://ci.chromium.org/b/bucket-123",
                "--bots",
                "the-bot",
                "--product-versions",
                "2",
            ]
        )

    def test_run_converter_from_env(self) -> None:
        """
        Test case that ensures that we correctly run the Converter with env data
        """
        subprocess_check_call: mock.Mock = mock.Mock()
        env = {
            "CATAPULT_DASHBOARD_MASTER": "the-master",
            "CATAPULT_DASHBOARD_BOT": "the-bot",
            "BUILDBUCKET_ID": "bucket-123",
            "BUILD_CREATE_TIME": "98765",
            "RELEASE_VERSION": "2",
        }
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._empty_fuchsia_perf_json],
                _EMPTY_EXPECTED_METRICS_FILE,
                env=env,
                subprocess_check_call=subprocess_check_call,
            )
        )

        converter.run()

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_output_path,
                "--execution-timestamp-ms",
                "98765",
                "--masters",
                "the-master",
                "--log-url",
                "https://ci.chromium.org/b/bucket-123",
                "--bots",
                "the-bot",
                "--product-versions",
                "2",
            ]
        )

    def test_run_converter_reject_mismatch_metrics(
        self,
    ) -> None:
        """
        Test case that ensures that we correctly validate the expected metrics
        """
        subprocess_check_call: mock.Mock = mock.Mock()
        with self.assertRaises(ValueError) as context:
            self.make_catapult_converter_for_test(
                [self._mismatch_metrics_fuchsia_perf_json],
                _EXPECTED_METRICS_FILE,
                env={},
                subprocess_check_call=subprocess_check_call,
            )
        self.assertIn(
            (
                " fuchsia.my.benchmark: metric_1\n"
                "-fuchsia.my.benchmark: metric_2\n"
                "-fuchsia.my.benchmark: metric_3\n"
                " fuchsia.my.benchmark: metric_4 [optional]\n"
                "+fuchsia.my.benchmark: unexpected\n"
            ),
            str(context.exception),
        )

        self.assertFalse(subprocess_check_call.called)

    def test_run_converter_accept_expected_metrics(
        self,
    ) -> None:
        """Test case that ensures that we correctly validate the expected metrics"""
        subprocess_check_call: mock.Mock = mock.Mock()
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._test_fuchsia_perf_json],
                _EXPECTED_METRICS_FILE,
                env={},
                subprocess_check_call=subprocess_check_call,
            )
        )
        converter.run()

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_local_output_path,
                "--execution-timestamp-ms",
                "12345000",
                "--masters",
                "local-master",
                "--log-url",
                "http://ci.example.com/build/300",
                "--bots",
                "local-bot",
            ]
        )

    def test_run_converter_writes_expectations_to_dir(
        self,
    ) -> None:
        """
        Test case that ensures that we correctly validate the expected metrics
        """
        subprocess_check_call: mock.Mock = mock.Mock()
        with tempfile.TemporaryDirectory() as tmpdir:
            converter: publish.CatapultConverter = self.make_catapult_converter_for_test(
                [self._mismatch_metrics_fuchsia_perf_json],
                _EXPECTED_METRICS_FILE,
                env={
                    publish.ENV_FUCHSIA_EXPECTED_METRIC_NAMES_DEST_DIR: tmpdir,
                },
                subprocess_check_call=subprocess_check_call,
            )
            converter.run()
            with open(os.path.join(tmpdir, _EXPECTED_METRICS_FILE), "r") as f:
                contents = f.read()
                self.assertEqual(
                    contents,
                    "fuchsia.my.benchmark: metric_1\nfuchsia.my.benchmark: unexpected\n",
                )

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_local_output_path,
                "--execution-timestamp-ms",
                "12345000",
                "--masters",
                "local-master",
                "--log-url",
                "http://ci.example.com/build/300",
                "--bots",
                "local-bot",
            ]
        )

    def test_run_converter_rejects_files_with_invalid_test_suites(
        self,
    ) -> None:
        """
        Test case that ensures that we correctly validate the expected metrics
        """
        subprocess_check_call: mock.Mock = mock.Mock()
        with self.assertRaises(ValueError) as context:
            self.make_catapult_converter_for_test(
                [self._invalid_suite_fuchsia_perf_json],
                _EXPECTED_METRICS_FILE,
                env={},
                subprocess_check_call=subprocess_check_call,
            )
        self.assertTrue(
            '"invalid_test_suite_name" does not match' in str(context.exception)
        )
        subprocess_check_call.assert_not_called()

    def test_integration_with_real_catapult_binary(self) -> None:
        """
        Test case that ensures that a call to the real coverage bin succeeds.
        """
        fuchsiaperf_data = [
            {
                "test_suite": "fuchsia.example",
                "label": "ExampleMetric1",
                "values": [10 + random.uniform(0, 1)],
                "unit": "ms",
            },
        ]
        expected_metrics = "fuchsia.example: ExampleMetric1"
        test_perf_file = os.path.join(
            self._temp_dir.name, "test.fuchsiaperf.json"
        )
        with open(test_perf_file, "w") as f:
            f.write(json.dumps(fuchsiaperf_data, indent=4))

        expected_metrics_file = os.path.join(
            self._temp_dir.name, "fuchsia.example.txt"
        )
        with open(expected_metrics_file, "w") as f:
            f.write(expected_metrics)

        converter: publish.CatapultConverter = (
            publish.CatapultConverter.from_env(
                [test_perf_file],
                expected_metrics_file,
                env={
                    publish.ENV_RELEASE_VERSION: "1",
                },
            )
        )

        converter.run()
        self.assertTrue(os.path.isfile(self._expected_local_output_path))
        self.assertFalse(os.path.isfile(self._expected_output_path))

    def test_converter_with_summarization_disabled(self) -> None:
        """Test case that ensures we correctly run the Converter with summarization disabled."""
        with open(self._test_fuchsia_perf_json, "r") as f:
            input_perf_json = json.load(f)

        subprocess_check_call: mock.Mock = mock.Mock()
        converter: publish.CatapultConverter = (
            self.make_catapult_converter_for_test(
                [self._test_fuchsia_perf_json],
                self._expected_metrics_no_summarize_txt,
                env={
                    publish.ENV_RELEASE_VERSION: "1",
                },
                subprocess_check_call=subprocess_check_call,
            )
        )

        self.assertTrue(os.path.isfile(self._expected_input_path))
        with open(self._expected_input_path, "r") as f:
            final_perf_json = json.load(f)
        self.assertEqual(final_perf_json, input_perf_json)

        converter.run()

        subprocess_check_call.assert_called_with(
            [
                _CATAPULT_CONVERTER_VALIDATOR,
                "--input",
                self._expected_input_path,
                "--output",
                self._expected_local_output_path,
                "--execution-timestamp-ms",
                "12345000",
                "--masters",
                "local-master",
                "--log-url",
                "http://ci.example.com/build/300",
                "--bots",
                "local-bot",
                "--product-versions",
                "1",
            ]
        )

    def _init_file(self, filename: str, contents: str) -> str:
        file_path = os.path.join(self._temp_dir.name, filename)
        with open(file_path, "w") as f:
            f.write(contents)
        return file_path
