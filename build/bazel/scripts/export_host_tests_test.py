#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Unit-test suite for export_host_tests.py."""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import export_host_tests


class HostInfoTest(unittest.TestCase):
    def test_HostInfo(self):
        host_info = export_host_tests.HostInfo()

        expected_os = sys.platform
        if expected_os == "darwin":
            expected_os = "mac"

        expected_cpu = os.uname().machine
        if expected_cpu == "x86_64":
            expected_cpu = "x64"
        elif expected_cpu == "aarch64":
            expected_cpu = "arm64"

        # "linux" -> "Linux", "mac" -> "Mac"
        expected_dimensions_os = expected_os[0].upper() + expected_os[1:]

        self.assertEqual(host_info.os, expected_os)
        self.assertEqual(host_info.cpu, expected_cpu)
        self.assertListEqual(
            host_info.test_json_environments,
            [
                {
                    "dimensions": {
                        "cpu": host_info.cpu,
                        "os": expected_dimensions_os,
                    }
                }
            ],
        )


class HardlinkOrCopyFileTest(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.dir = Path(self._td.name)
        self.src_dir = self.dir / "source" / "dir"
        self.src_dir.mkdir(parents=True)
        self.dst_dir = self.dir / "dest" / "dir"

    def tearDown(self):
        self._td.cleanup()

    def test_file(self):
        src_file = self.src_dir / "source_file"
        src_file.write_text("Hey!")
        dst_file = self.dst_dir / "target_file"

        export_host_tests.hardlink_or_copy_file(src_file, dst_file)
        self.assertTrue(dst_file.is_file())

        # Verify that both points to the same i-node
        src_info = src_file.stat()
        dst_info = dst_file.stat()
        self.assertEqual(src_info.st_dev, dst_info.st_dev)
        self.assertEqual(src_info.st_ino, dst_info.st_ino)

    def test_absolute_symlink(self):
        src_link = self.src_dir / "source_link"
        src_link.symlink_to(self.dir / "nonexistent" / "path")
        dst_link = self.dst_dir / "target_link"

        export_host_tests.hardlink_or_copy_file(src_link, dst_link)
        self.assertTrue(dst_link.is_symlink())
        src_target = src_link.readlink()
        dst_target = dst_link.readlink()
        self.assertTrue(src_target.is_absolute())
        self.assertFalse(dst_target.is_absolute())
        self.assertEqual(src_target, (dst_link.parent / dst_target).resolve())

    def test_relative_symlink(self):
        src_link = self.src_dir / "source_link"
        src_link.symlink_to(
            os.path.relpath(self.dir / "nonexistent" / "path", src_link.parent)
        )
        dst_link = self.dst_dir / "target_link"

        export_host_tests.hardlink_or_copy_file(src_link, dst_link)
        self.assertTrue(dst_link.is_symlink())
        src_target = src_link.readlink()
        dst_target = dst_link.readlink()
        self.assertFalse(src_target.is_absolute())
        self.assertFalse(dst_target.is_absolute())
        self.assertEqual(
            (src_link.parent / src_target).resolve(),
            (dst_link.parent / dst_target).resolve(),
        )

    def test_directory(self):
        with self.assertRaises(ValueError) as cm:
            export_host_tests.hardlink_or_copy_file(self.src_dir, self.dst_dir)

        self.assertEqual(
            str(cm.exception),
            f"Cannot hard-link or copy directory: {self.src_dir}",
        )


class FilterBazelBinPathTest(unittest.TestCase):
    def test_good_paths(self):
        TEST_CASES = {
            "bazel-out/something/bin/foo": "foo",
            "bazel-out/something-else/bin/foo/bar": "foo/bar",
            "bazel-out/k8-fastbuild-ST-228372328ef8a/bin/build/tests/prog": "build/tests/prog",
            "bazel-out/k8-fastbuild/bin/bazel-out/k8-fastbuild/bin/bar": "bazel-out/k8-fastbuild/bin/bar",
        }
        for input, expected in TEST_CASES.items():
            self.assertEqual(
                export_host_tests.filter_bazel_bin_path(input), expected, input
            )

    def test_bad_paths(self):
        TEST_CASES = [
            "src/foo",
            "bazel-out/src/foo",
            "bazel-out/bin/src/foo",
            "execroot/_main/bazel-out/k8-fastbuild/bin/foo",
            "./bazel-out/k8-fastbuild/bin/foo/bar",
        ]
        for path in TEST_CASES:
            with self.assertRaises(ValueError) as cm:
                export_host_tests.filter_bazel_bin_path(path)

            self.assertEqual(
                str(cm.exception),
                f"Expected execroot-relative artifact path, got [{path}]",
            )


class HostTestInfoTest(unittest.TestCase):
    TEST_INPUT = {
        "label": "@//src/foo:test_program",
        "executable": "bazel-out/k8-fastbuild/bin/src/foo/test_program",
        "runfiles_manifest": "bazel-out/k8-fastbuild/bin/src/foo/test_program.runfiles/MANIFEST",
    }

    def test_construction_with_bad_label(self):
        host_info = export_host_tests.HostInfo()
        test_input = self.TEST_INPUT.copy()
        test_input["label"] = "//src/foo:test_program"
        with self.assertRaises(AssertionError) as cm:
            export_host_tests.HostTestInfo(test_input, host_info)

        self.assertEqual(
            str(cm.exception),
            "Invalid Bazel target label does not begin with @: //src/foo:test_program",
        )

    def test_generate_tests_json_entry_without_export_dir(self):
        host_info = export_host_tests.HostInfo()
        test_info = export_host_tests.HostTestInfo(self.TEST_INPUT, host_info)
        self.assertDictEqual(
            test_info.generate_tests_json_entry(),
            {
                "environments": host_info.test_json_environments,
                "expects_ssh": False,
                "label": "@//src/foo:test_program",
                "name": "src/foo/test_program",
                "path": "src/foo/test_program",
                "runtime_deps": "src/foo/test_program.runtime_deps.json",
                "os": host_info.os,
                "cpu": host_info.cpu,
            },
        )

    def test_generate_tests_json_entry_with_export_subdir(self):
        host_info = export_host_tests.HostInfo()
        test_info = export_host_tests.HostTestInfo(
            self.TEST_INPUT, host_info, "bazel_host_tests"
        )
        self.assertDictEqual(
            test_info.generate_tests_json_entry(),
            {
                "environments": host_info.test_json_environments,
                "expects_ssh": False,
                "label": "@//src/foo:test_program",
                "name": "bazel_host_tests/src/foo/test_program",
                "path": "bazel_host_tests/src/foo/test_program",
                "runtime_deps": "bazel_host_tests/src/foo/test_program.runtime_deps.json",
                "os": host_info.os,
                "cpu": host_info.cpu,
            },
        )

    def test_export_to(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            execroot = Path(temp_dir) / "execroot"
            build_dir = Path(temp_dir) / "build_dir"

            # Generate a fake repo mapping file.
            bazel_bin_shortpath = "bazel-out/k8-fastbuild/bin"
            bazel_bin_path = execroot / bazel_bin_shortpath
            repo_mapping_path = (
                bazel_bin_path / "src/foo/test_program.repo_mapping"
            )
            repo_mapping_path.parent.mkdir(parents=True)
            repo_mapping_path.write_text(",project,_main")

            # Generate a fake manifest
            manifest_path = execroot / self.TEST_INPUT["runfiles_manifest"]
            manifest_path.parent.mkdir(parents=True)
            manifest_path.write_text(
                "\n".join(
                    sorted(
                        [
                            "_main/data/file {bazel_bin_shortpath}/data/file",
                            "_main/src/foo/test_program {bazel_bin_shortpath}/src/foo/test_program",
                            f"_repo_mapping {os.path.relpath(repo_mapping_path, execroot)}",
                        ]
                    )
                )
            )

            # Generate a fake executable
            exec_path = bazel_bin_path / "src/foo/test_program"
            exec_path.write_text("#!/bin/sh\necho ok\n")

            host_info = export_host_tests.HostInfo()
            test_info = export_host_tests.HostTestInfo(
                self.TEST_INPUT, host_info, "exported"
            )

            # Do the export
            test_info.export_to(build_dir, execroot)

            # Now verify results.

            export_root = build_dir / "exported" / "src" / "foo"

            exported_exec = export_root / "test_program"
            self.assertTrue(exported_exec.is_file())
            self.assertEqual(
                exported_exec.stat(),
                exec_path.stat(),
                f"Exported test should be a hard-link: {exported_exec}",
            )

            def resolve_one_symlink(path: Path) -> Path:
                target = path.readlink()
                if not target.is_absolute():
                    target = (path.parent / target).resolve()
                return target

            def assertIsSymlinkTo(src_path: Path, dst_path: Path):
                self.assertTrue(
                    src_path.is_symlink(), f"Not a symlink: {src_path}"
                )
                src_target = resolve_one_symlink(src_path)
                self.assertTrue(src_target, dst_path)

            assertIsSymlinkTo(
                export_root / "test_program.repo_mapping", repo_mapping_path
            )

            exported_manifest_path = (
                export_root / "test_program.runfiles/MANIFEST"
            )
            self.assertTrue(exported_manifest_path.is_file())

            assertIsSymlinkTo(
                export_root / "test_program.runfiles_manifest",
                exported_manifest_path,
            )

            self.assertEqual(
                exported_manifest_path.read_text(),
                "_main/data/file exported/src/foo/test_program.runfiles/_main/data/file\n"
                + "_main/src/foo/test_program exported/src/foo/test_program.runfiles/_main/src/foo/test_program\n"
                + "_repo_mapping exported/src/foo/test_program.runfiles/_repo_mapping\n",
            )

            runtime_deps_path = export_root / "test_program.runtime_deps.json"
            self.assertTrue(runtime_deps_path.is_file())
            self.assertListEqual(
                json.loads(runtime_deps_path.read_text()),
                [
                    "exported/src/foo/test_program.runfiles/_main/data/file",
                    "exported/src/foo/test_program.runfiles/_main/src/foo/test_program",
                    "exported/src/foo/test_program.runfiles/_repo_mapping",
                    "exported/src/foo/test_program.runfiles/MANIFEST",
                    "exported/src/foo/test_program.runfiles_manifest",
                    "exported/src/foo/test_program.repo_mapping",
                ],
            )


if __name__ == "__main__":
    unittest.main()
