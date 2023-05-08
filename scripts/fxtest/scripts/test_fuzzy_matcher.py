# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import fuzzy_matcher
import io
import json
import os
import tempfile
import unittest


class PreserveEnvAndCaptureOutputTestCase(unittest.TestCase):
    def setUp(self):
        self._old_fuchsia_dir = os.getenv("FUCHSIA_DIR")
        self.stdout = io.StringIO()
        self._context = contextlib.redirect_stdout(self.stdout)
        self._context.__enter__()

    def tearDown(self):
        if self._old_fuchsia_dir:
            os.environ["FUCHSIA_DIR"] = self._old_fuchsia_dir
        self._context.__exit__(None, None, None)
        return super().tearDown()


class TestSearchLocations(PreserveEnvAndCaptureOutputTestCase):
    def test_environment_variable_unset(self):
        del os.environ["FUCHSIA_DIR"]

        with self.assertRaises(Exception) as ex:
            fuzzy_matcher.create_search_locations()

        self.assertEqual(
            str(ex.exception), "Environment variable FUCHSIA_DIR must be set"
        )

    def test_not_a_file(self):
        with tempfile.TemporaryDirectory() as dir:
            path = os.path.join(dir, "tmpfile")
            with open(path, "w"):
                pass
            os.environ["FUCHSIA_DIR"] = str(path)

            with self.assertRaises(Exception) as ex:
                fuzzy_matcher.create_search_locations()

            self.assertEqual(str(ex.exception), f"Path {path} should be a directory")

    def test_missing_tests_json(self):
        with tempfile.TemporaryDirectory() as dir:
            with open(os.path.join(dir, ".fx-build-dir"), "w") as f:
                f.write("out/other")

            os.makedirs(os.path.join(dir, "out", "other"))

            os.environ["FUCHSIA_DIR"] = str(dir)

            with self.assertRaises(Exception) as ex:
                fuzzy_matcher.create_search_locations()

            expected = os.path.join(dir, "out", "other", "tests.json")

            self.assertEqual(
                str(ex.exception), f"Expected to find a test list file at {expected}"
            )

    def test_success(self):
        with tempfile.TemporaryDirectory() as dir:
            path = os.path.join(dir, ".fx-build-dir")
            with open(os.path.join(dir, ".fx-build-dir"), "w") as f:
                f.write("out/other")
            os.makedirs(os.path.join(dir, "out", "other"))
            with open(os.path.join(dir, "out", "other", "tests.json"), "w") as f:
                pass

            os.environ["FUCHSIA_DIR"] = str(dir)

            locations = fuzzy_matcher.create_search_locations()
            self.assertEqual(locations.fuchsia_directory, dir)
            self.assertEqual(
                locations.tests_json_file,
                os.path.join(dir, "out", "other", "tests.json"),
            )
            self.assertNotEqual("", str(locations))


class TestTestsFileMatcher(unittest.TestCase):
    def _write_names(self, dir, names) -> str:
        path = os.path.join(dir, "tests.json")
        with open(path, "w") as f:
            l = [{"test": {"name": name}} for name in names]
            json.dump(l, f)
        return path

    def test_empty_file(self):
        with tempfile.TemporaryDirectory() as dir:
            path = self._write_names(dir, [])
            tests_matcher = fuzzy_matcher.TestsFileMatcher(path)
            matcher = fuzzy_matcher.Matcher(threshold=0.75)
            self.assertEqual(tests_matcher.find_matches("foo", matcher), [])

    def test_exact_matches(self):
        with tempfile.TemporaryDirectory() as dir:
            path = self._write_names(
                dir,
                [
                    "fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm",
                    "host_test/my-host-test",
                ],
            )
            tests_matcher = fuzzy_matcher.TestsFileMatcher(path)
            matcher = fuzzy_matcher.Matcher(threshold=1)
            self.assertEqual(
                [
                    val.matched_name
                    for val in tests_matcher.find_matches("my_component", matcher)
                ],
                ["my-component"],
            )
            self.assertEqual(
                [
                    val.matched_name
                    for val in tests_matcher.find_matches("my_package", matcher)
                ],
                ["my-package"],
            )
            self.assertEqual(
                [
                    val.matched_name
                    for val in tests_matcher.find_matches("my_host_test", matcher)
                ],
                ["my-host-test"],
            )


TEST_PACKAGE = (
    lambda x: f"""
fuchsia_test_package("{x}") {{

}}
"""
)

TEST_COMPONENT = (
    lambda x: f"""
fuchsia_test_component("{x}") {{

}}
"""
)

TEST_COMPONENT_WITH_COMPONENT_NAME = (
    lambda x, name: f"""
fuchsia_test_component("{x}") {{
    component_name = "{name}"
}}
"""
)

TEST_PACKAGE_WITH_PACKAGE_NAME = (
    lambda x, name: f"""
fuchsia_test_package("{x}") {{
    package_name = "{name}"
}}
"""
)

TEST_PACKAGE_WITH_COMPONENT_NAME = (
    lambda x, name: f"""
fuchsia_test_package("{x}") {{
    component_name = "{name}"
}}
"""
)

TEST_PACKAGE_WITH_TEST_COMPONENTS = (
    lambda x, components: f"""
fuchsia_test_package("{x}") {{
    test_components = [
        {",".join([f'"{name}"' for name in components])}
    ]
}}
"""
)


class TestBuildFileMatcher(unittest.TestCase):
    def test_simple_packages(self):
        with tempfile.TemporaryDirectory() as dir:
            os.makedirs(os.path.join(dir, "src"))
            with open(os.path.join(dir, "src", "BUILD.gn"), "w") as f:
                f.write(TEST_PACKAGE("my-test-package"))
                f.write(
                    TEST_PACKAGE_WITH_PACKAGE_NAME("other-package", "other-real-name")
                )
                f.write(
                    TEST_PACKAGE_WITH_COMPONENT_NAME(
                        "yet-another-package", "yet-another-real-name"
                    )
                )

            build_matcher = fuzzy_matcher.BuildFileMatcher(dir)
            matcher = fuzzy_matcher.Matcher(threshold=1)

            self.assertEqual(
                [
                    (val.matched_name, val.full_suggestion)
                    for val in build_matcher.find_matches("my-test-package", matcher)
                ],
                [("my-test-package", "--with //src:my-test-package")],
            )

            self.assertEqual(
                [
                    (val.matched_name, val.full_suggestion)
                    for val in build_matcher.find_matches("other-real-name", matcher)
                ],
                [("other-real-name", "--with //src:other-package")],
            )

            self.assertEqual(
                [
                    (val.matched_name, val.full_suggestion)
                    for val in build_matcher.find_matches(
                        "yet-another-real-name", matcher
                    )
                ],
                [("yet-another-real-name", "--with //src:yet-another-package")],
            )

    def test_packages_with_components(self):
        with tempfile.TemporaryDirectory() as dir:
            os.makedirs(os.path.join(dir, "src", "nested"))
            with open(os.path.join(dir, "src", "nested", "BUILD.gn"), "w") as f:
                f.write(TEST_COMPONENT("my-test-component"))
                f.write(
                    TEST_COMPONENT_WITH_COMPONENT_NAME(
                        "another-component", "component-real-name"
                    )
                )
                f.write(
                    TEST_PACKAGE_WITH_TEST_COMPONENTS(
                        "test-package", [":my-test-component", ":another-component"]
                    )
                )

            build_matcher = fuzzy_matcher.BuildFileMatcher(dir)
            matcher = fuzzy_matcher.Matcher(threshold=1)

            self.assertEqual(
                [
                    (val.matched_name, val.full_suggestion)
                    for val in build_matcher.find_matches("my_test_component", matcher)
                ],
                [("my-test-component", "--with //src/nested:test-package")],
            )

            self.assertEqual(
                [
                    (val.matched_name, val.full_suggestion)
                    for val in build_matcher.find_matches(
                        "component_real_name", matcher
                    )
                ],
                [("component-real-name", "--with //src/nested:test-package")],
            )


class TestTimingTracker(PreserveEnvAndCaptureOutputTestCase):
    def test_timing(self):
        fuzzy_matcher.TimingTracker.reset()

        with fuzzy_matcher.TimingTracker("Test timings"):
            pass
        with fuzzy_matcher.TimingTracker("Test again"):
            pass

        fuzzy_matcher.TimingTracker.print_timings()

        lines = self.stdout.getvalue().strip().split("\n")
        self.assertEqual(lines[0], "Debug timings:")
        self.assertRegex(lines[1], r"\s+Test timings\s+\d+\.\d\d\dms$")
        self.assertRegex(lines[2], r"\s+Test again\s+\d+\.\d\d\dms$")

        with fuzzy_matcher.TimingTracker("In progress"):
            # Ensure we omit in progress readings
            fuzzy_matcher.TimingTracker.print_timings()
            lines2 = self.stdout.getvalue().strip().split("\n")
            self.assertListEqual(lines, lines2[len(lines) :])


class TestCommand(PreserveEnvAndCaptureOutputTestCase):
    def setUp(self):
        super().setUp()
        self.dir = tempfile.TemporaryDirectory()
        with open(os.path.join(self.dir.name, ".fx-build-dir"), "w") as f:
            f.write("out/default")
        os.makedirs(os.path.join(self.dir.name, "out", "default"))
        os.makedirs(os.path.join(self.dir.name, "src", "nested"))
        with open(
            os.path.join(self.dir.name, "out", "default", "tests.json"), "w"
        ) as f:
            json.dump(
                [
                    {
                        "test": {
                            "name": "fuchsia-pkg://fuchsia.com/foo-tests#meta/foo-test-component.cm"
                        }
                    },
                    {"test": {"name": "host_x64/local_script_test"}},
                ],
                f,
            )
        with open(os.path.join(self.dir.name, "src", "nested", "BUILD.gn"), "w") as f:
            f.write(TEST_COMPONENT("foo-test-component"))
            f.write(
                TEST_PACKAGE_WITH_TEST_COMPONENTS("foo-tests", [":foo-test-component"])
            )
        with open(os.path.join(self.dir.name, "src", "BUILD.gn"), "w") as f:
            f.write(TEST_PACKAGE_WITH_COMPONENT_NAME("tests", "kernel-tests"))
            f.write(
                TEST_PACKAGE_WITH_PACKAGE_NAME("component-tests", "my-component-tests")
            )
            f.write(TEST_PACKAGE("integration-tests"))

        os.environ["FUCHSIA_DIR"] = str(self.dir.name)

    def tearDown(self):
        self.dir.cleanup()
        return super().tearDown()

    def test_bad_arguments(self):
        with self.assertRaises(Exception) as ex:
            fuzzy_matcher.main(["foo", "--threshold", "3"])
        self.assertEqual(str(ex.exception), "--threshold must be between 0 and 1")

    def test_without_matches(self):
        fuzzy_matcher.main(["afkdjsflkejkgh"])
        self.assertTrue(
            "No matching tests" in self.stdout.getvalue(),
            "Could not find expected string in " + self.stdout.getvalue(),
        )

    def test_component_match(self):
        fuzzy_matcher.main(["foo-test-component", "--threshold", "1", "--no-color"])

        self.assertEqual(
            self.stdout.getvalue().strip(),
            """
foo-test-component (100.00% similar)
    Build includes: fuchsia-pkg://fuchsia.com/foo-tests#meta/foo-test-component.cm
""".strip(),
        )

    def test_package_match(self):
        fuzzy_matcher.main(["kernel", "--threshold", ".75", "--no-color"])

        self.assertEqual(
            self.stdout.getvalue().strip(),
            """
kernel-tests (90.00% similar)
    --with //src:tests
""".strip(),
        )

    def test_multi_match(self):
        fuzzy_matcher.main(
            ["tests", "--threshold", ".2", "--no-color", "--max-results=3"]
        )

        self.assertEqual(
            self.stdout.getvalue().strip(),
            """
foo-test-component (67.41% similar)
    Build includes: fuchsia-pkg://fuchsia.com/foo-tests#meta/foo-test-component.cm
kernel-tests (61.67% similar)
    --with //src:tests
integration-tests (59.22% similar)
    --with //src:integration-tests
(3 more matches not shown)
""".strip(),
        )

    def test_with_without_tests_json_match(self):
        fuzzy_matcher.main(["foo-test-component", "--threshold", "1", "--no-color"])
        fuzzy_matcher.main(
            ["foo-test-component", "--threshold", "1", "--no-color", "--omit-test-file"]
        )

        self.assertEqual(
            self.stdout.getvalue().strip(),
            """
foo-test-component (100.00% similar)
    Build includes: fuchsia-pkg://fuchsia.com/foo-tests#meta/foo-test-component.cm
No matching tests could be found in your Fuchsia checkout.
""".strip(),
        )


if __name__ == "__main__":
    unittest.main()
