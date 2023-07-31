# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import shutil
import tempfile
import unittest
import unittest.mock as mock

import args
import environment


class TestExecutionEnvironment(unittest.TestCase):
    def _make_test_files(self, tmp):
        out_dir = os.path.join(tmp, "out", "foo")
        os.makedirs(out_dir)
        with open(os.path.join(tmp, ".fx-build-dir"), "w") as f:
            f.write(out_dir)

        open(os.path.join(out_dir, "tests.json"), "a").close()
        open(os.path.join(out_dir, "test-list.json"), "a").close()

    def test_process_environment(self):
        """Test that we can load and use an environment."""
        with tempfile.TemporaryDirectory() as tmp:
            self._make_test_files(tmp)

            out_dir = os.path.join(tmp, "out", "foo")
            default_flags = args.parse_args([])

            with mock.patch.dict(os.environ, {"FUCHSIA_DIR": tmp}):
                env = environment.ExecutionEnvironment.initialize_from_args(
                    default_flags
                )
                self.assertEqual(env.fuchsia_dir, tmp)
                self.assertEqual(env.out_dir, out_dir)
                self.assertTrue(
                    env.log_file and env.log_file.startswith(out_dir), str(env)
                )
                self.assertTrue(env.log_file and "fxtest" in env.log_file, str(env))
                self.assertEqual(
                    env.test_json_file, os.path.join(out_dir, "tests.json")
                )
                self.assertEqual(
                    env.test_list_file, os.path.join(out_dir, "test-list.json")
                )

                self.assertEqual(
                    env.relative_to_root(os.path.join(tmp, "foo", "bar")),
                    os.path.join("foo", "bar"),
                )

    def test_no_fuchsia_dir(self):
        with mock.patch.dict(os.environ, {"FUCHSIA_DIR": ""}):
            default_flags = args.parse_args([])
            self.assertRaisesRegex(
                environment.EnvironmentError,
                r"FUCHSIA_DIR",
                lambda: environment.ExecutionEnvironment.initialize_from_args(
                    default_flags
                ),
            )

    def test_missing_build_dir_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._make_test_files(tmp)
            os.remove(os.path.join(tmp, ".fx-build-dir"))

            with mock.patch.dict(os.environ, {"FUCHSIA_DIR": tmp}):
                default_flags = args.parse_args([])
                self.assertRaisesRegex(
                    environment.EnvironmentError,
                    r".fx-build-dir",
                    lambda: environment.ExecutionEnvironment.initialize_from_args(
                        default_flags
                    ),
                )

    def test_missing_build_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._make_test_files(tmp)
            shutil.rmtree(os.path.join(tmp, "out", "foo"))

            with mock.patch.dict(os.environ, {"FUCHSIA_DIR": tmp}):
                default_flags = args.parse_args([])
                self.assertRaisesRegex(
                    environment.EnvironmentError,
                    r"^Expected directory at.+out/foo$",
                    lambda: environment.ExecutionEnvironment.initialize_from_args(
                        default_flags
                    ),
                )

    def test_missing_tests_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._make_test_files(tmp)
            os.remove(os.path.join(tmp, "out", "foo", "tests.json"))

            with mock.patch.dict(os.environ, {"FUCHSIA_DIR": tmp}):
                default_flags = args.parse_args([])
                self.assertRaisesRegex(
                    environment.EnvironmentError,
                    r"tests.json",
                    lambda: environment.ExecutionEnvironment.initialize_from_args(
                        default_flags
                    ),
                )

    def test_missing_test_list_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._make_test_files(tmp)
            os.remove(os.path.join(tmp, "out", "foo", "test-list.json"))

            with mock.patch.dict(os.environ, {"FUCHSIA_DIR": tmp}):
                default_flags = args.parse_args([])
                self.assertRaisesRegex(
                    environment.EnvironmentError,
                    r"test-list.json",
                    lambda: environment.ExecutionEnvironment.initialize_from_args(
                        default_flags
                    ),
                )
