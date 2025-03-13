#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import workspace_utils
from workspace_utils import GnBuildArgs


class TestFindFuchsiaDir(unittest.TestCase):
    def test_find_fuchsia_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            not_fuchsia_dir = Path(tmp_dir) / "this_is_not_fuchsia"
            not_fuchsia_dir.mkdir()
            fuchsia_dir = Path(tmp_dir) / "this_is_fuchsia"
            fuchsia_dir.mkdir()
            (fuchsia_dir / ".jiri_manifest").write_text("")
            (fuchsia_dir / "src" / "foo").mkdir(parents=True)

            # Check function when searching from the current path.
            saved_cwd = os.getcwd()
            try:
                os.chdir(not_fuchsia_dir)
                with self.assertRaises(ValueError):
                    workspace_utils.find_fuchsia_dir()

                for path in (
                    fuchsia_dir,
                    fuchsia_dir / "src",
                    fuchsia_dir / "src" / "foo",
                ):
                    os.chdir(path)
                    self.assertEqual(
                        workspace_utils.find_fuchsia_dir(),
                        fuchsia_dir,
                        f"From {path}",
                    )

                # Ensure the result is absolute even if the starting path is relative.
                os.chdir(fuchsia_dir)
                for path in (Path("."), Path("src"), Path("src/foo")):
                    self.assertEqual(
                        workspace_utils.find_fuchsia_dir(path),
                        fuchsia_dir,
                        f"From {path}",
                    )

            finally:
                os.chdir(saved_cwd)

            # Check function when searching from a given path.
            with self.assertRaises(ValueError):
                workspace_utils.find_fuchsia_dir(not_fuchsia_dir)

            for path in (
                fuchsia_dir,
                fuchsia_dir / "src",
                fuchsia_dir / "src" / "foo",
            ):
                self.assertEqual(
                    workspace_utils.find_fuchsia_dir(path),
                    fuchsia_dir,
                    f"From {path}",
                )


class TestFindFxBuildDir(unittest.TestCase):
    def test_find_fx_build_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            fuchsia_dir = Path(tmp_dir)

            # No .fx-build-dir present -> Path()
            self.assertEqual(
                workspace_utils.find_fx_build_dir(fuchsia_dir), None
            )

            # Empty .fx-build-dir content -> Path()
            fx_build_dir_path = fuchsia_dir / ".fx-build-dir"
            fx_build_dir_path.write_text("")
            self.assertEqual(
                workspace_utils.find_fx_build_dir(fuchsia_dir), None
            )

            # Invalid .fx-build-dir content -> Path()
            fx_build_dir_path.write_text("does/not/exist\n")
            self.assertEqual(
                workspace_utils.find_fx_build_dir(fuchsia_dir), None
            )

            # Valid build directory.
            build_dir = fuchsia_dir / "some" / "build_dir"
            build_dir.mkdir(parents=True)

            fx_build_dir_path.write_text("some/build_dir\n")
            self.assertEqual(
                workspace_utils.find_fx_build_dir(fuchsia_dir), build_dir
            )


class TestFindBazelPaths(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self.fuchsia_dir = Path(self._td.name)
        config_dir = self.fuchsia_dir / "build" / "bazel" / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "main_workspace_top_dir").write_text("some/top/dir\n")

        self.build_dir = self.fuchsia_dir / "out" / "build_dir"
        self.launcher_path = self.build_dir / "some/top/dir/bazel"
        self.workspace_path = self.build_dir / "some/top/dir/workspace"

    def tearDown(self) -> None:
        self._td.cleanup()

    def test_find_bazel_launcher_path(self) -> None:
        self.assertEqual(
            workspace_utils.find_bazel_launcher_path(
                self.fuchsia_dir, self.build_dir
            ),
            None,
        )

        self.launcher_path.parent.mkdir(parents=True)
        self.launcher_path.write_text("!")
        self.assertEqual(
            workspace_utils.find_bazel_launcher_path(
                self.fuchsia_dir, self.build_dir
            ),
            self.launcher_path,
        )

    def test_find_bazel_workspace_path(self) -> None:
        self.assertEqual(
            workspace_utils.find_bazel_workspace_path(
                self.fuchsia_dir, self.build_dir
            ),
            None,
        )

        self.workspace_path.mkdir(parents=True)
        self.assertEqual(
            workspace_utils.find_bazel_workspace_path(
                self.fuchsia_dir, self.build_dir
            ),
            self.workspace_path,
        )


class TestFindBazelWorkspacePath(unittest.TestCase):
    def test_find_bazel_workspace_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            fuchsia_dir = Path(tmp_dir)
            config_dir = fuchsia_dir / "build" / "bazel" / "config"
            config_dir.mkdir(parents=True)
            (config_dir / "main_workspace_top_dir").write_text("some/top/dir\n")

            build_dir = fuchsia_dir / "out" / "build_dir"
            launcher_path = build_dir / "some/top/dir/bazel"

            self.assertEqual(
                workspace_utils.find_bazel_launcher_path(
                    fuchsia_dir, build_dir
                ),
                None,
            )

            launcher_path.parent.mkdir(parents=True)
            launcher_path.write_text("!")
            self.assertEqual(
                workspace_utils.find_bazel_launcher_path(
                    fuchsia_dir, build_dir
                ),
                launcher_path,
            )


class TestGetBazelRelativeTopDir(unittest.TestCase):
    def test_get_bazel_relative_topdir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            fuchsia_dir = Path(tmp_dir)
            config_dir = fuchsia_dir / "build" / "bazel" / "config"
            config_dir.mkdir(parents=True)

            main_config = config_dir / "main_workspace_top_dir"
            main_config.write_text("gen/test/bazel_workspace\n")

            alt_config = config_dir / "alt_workspace_top_dir"
            alt_config.write_text(" alternative_workspace \n")

            topdir, input_files = workspace_utils.get_bazel_relative_topdir(
                fuchsia_dir, "main"
            )
            self.assertEqual(topdir, "gen/test/bazel_workspace")
            self.assertListEqual(list(input_files), [main_config])

            topdir, input_files = workspace_utils.get_bazel_relative_topdir(
                str(fuchsia_dir), "main"
            )
            self.assertEqual(topdir, "gen/test/bazel_workspace")
            self.assertListEqual(list(input_files), [main_config])

            topdir, input_files = workspace_utils.get_bazel_relative_topdir(
                str(fuchsia_dir), "alt"
            )
            self.assertEqual(topdir, "alternative_workspace")
            self.assertListEqual(list(input_files), [alt_config])


class TestWorkspaceShouldExcludeFile(unittest.TestCase):
    def test_workspace_should_exclude_file(self) -> None:
        _EXPECTED_EXCLUDED_PATHS = [
            "out",
            ".jiri",
            ".fx",
            ".git",
            "bazel-bin",
            "bazel-repos",
            "bazel-out",
            "bazel-workspace",
        ]
        for path in _EXPECTED_EXCLUDED_PATHS:
            self.assertTrue(
                workspace_utils.workspace_should_exclude_file(path),
                msg=f"For path [{path}]",
            )

        _EXPECTED_INCLUDED_PATHS = [
            "out2",
            "src",
            ".clang-format",
            ".gn",
        ]
        for path in _EXPECTED_INCLUDED_PATHS:
            self.assertFalse(
                workspace_utils.workspace_should_exclude_file(path),
                msg=f"For path [{path}]",
            )


class TestForceSymlink(unittest.TestCase):
    def test_force_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            tmp_path = Path(tmp_dir).resolve()

            # Create a new symlink, then ensure its embedded target is relative.
            # The target doesn't need to exist.
            target_path = tmp_path / "target" / "file"
            link_path = tmp_path / "links" / "dir" / "symlink"

            workspace_utils.force_symlink(link_path, target_path)

            self.assertTrue(link_path.is_symlink())
            self.assertEqual(str(link_path.readlink()), "../../target/file")

            # Update the target to a new path, verify the symlink was updated.
            target_path = tmp_path / "target" / "new_file"

            workspace_utils.force_symlink(link_path, target_path)
            self.assertTrue(link_path.is_symlink())
            self.assertEqual(str(link_path.readlink()), "../../target/new_file")


class TestGeneratedWorkspaceFiles(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self.out = Path(self._td.name)
        (self.out / "elephant").write_text("trumpet!")

    def tearDown(self) -> None:
        self._td.cleanup()

    def test_with_no_file_hasher(self) -> None:
        ws_files = workspace_utils.GeneratedWorkspaceFiles()
        ws_files.record_file_content("zoo/lion", "roar!")
        ws_files.record_symlink("zoo/elephant", self.out / "elephant")
        ws_files.record_input_file_hash("no/such/file/exists")

        expected_json = r"""{
  "zoo/elephant": {
    "target": "@OUT@/elephant",
    "type": "symlink"
  },
  "zoo/lion": {
    "content": "roar!",
    "type": "file"
  }
}""".replace(
            "@OUT@", str(self.out)
        )

        self.assertEqual(ws_files.to_json(), expected_json)

        ws_files.write(self.out / "workspace")
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "lion").read_text(), "roar!"
        )
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "elephant").read_text(),
            "trumpet!",
        )
        self.assertEqual(
            str((self.out / "workspace" / "zoo" / "elephant").readlink()),
            "../../elephant",
        )

    def test_with_file_hasher(self) -> None:
        ws_files = workspace_utils.GeneratedWorkspaceFiles()
        ws_files.set_file_hasher(lambda path: f"SHA256[{path}]")
        ws_files.record_file_content("zoo/lion", "roar!")
        ws_files.record_symlink("zoo/elephant", self.out / "elephant")
        ws_files.record_input_file_hash("no/such/file/exists")

        expected_json = r"""{
  "no/such/file/exists": {
    "hash": "SHA256[no/such/file/exists]",
    "type": "input_file"
  },
  "zoo/elephant": {
    "target": "@OUT@/elephant",
    "type": "symlink"
  },
  "zoo/lion": {
    "content": "roar!",
    "type": "file"
  }
}""".replace(
            "@OUT@", str(self.out)
        )

        self.assertEqual(ws_files.to_json(), expected_json)

        ws_files.write(self.out / "workspace")
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "lion").read_text(), "roar!"
        )
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "elephant").read_text(),
            "trumpet!",
        )
        self.assertEqual(
            str((self.out / "workspace" / "zoo" / "elephant").readlink()),
            "../../elephant",
        )
        self.assertFalse(
            (
                self.out / "workspace" / "no" / "such" / "file" / "exists"
            ).exists()
        )

    def test_update_if_needed(self) -> None:
        ws_files = workspace_utils.GeneratedWorkspaceFiles()
        ws_files.set_file_hasher(lambda path: f"SHA256[{path}]")
        ws_files.record_file_content("zoo/lion", "roar!")
        ws_files.record_symlink("zoo/elephant", self.out / "elephant")
        ws_files.record_input_file_hash("no/such/file/exists")

        ws_dir = self.out / "workspace"
        ws_manifest = self.out / "manifest"

        # The manifest file does not exist, so update the directory.
        self.assertTrue(ws_files.update_if_needed(ws_dir, ws_manifest))

        # A second call with the same inputs should do nothing.
        self.assertFalse(ws_files.update_if_needed(ws_dir, ws_manifest))

        # Modify the manifest file to an empty dict, verify that the output
        # directory is not empty.
        ws_manifest.write_text("{}")
        self.assertTrue(ws_files.update_if_needed(ws_dir, ws_manifest))
        self.assertListEqual(os.listdir(ws_dir), ["zoo"])

        # Now update the workspace to be empty. Verify that the manifest is now just "{}"
        # and that the output directory is empty.
        empty_files = workspace_utils.GeneratedWorkspaceFiles()
        self.assertTrue(empty_files.update_if_needed(ws_dir, ws_manifest))
        self.assertEqual(ws_manifest.read_text(), "{}")
        self.assertListEqual(os.listdir(ws_dir), [])


class RemoveDirTests(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = self._td.name

    def tearDown(self) -> None:
        self._td.cleanup()

    def _run_checks(
        self, test_name: str, top_dir: str, all_paths: set[str]
    ) -> None:
        for p in all_paths:
            self.assertTrue(
                os.path.exists(p),
                f"{test_name} setup failure: {p} does not exist after test dir creation",
            )

        workspace_utils.remove_dir(top_dir)

        for p in all_paths:
            self.assertFalse(
                os.path.exists(p),
                f"{test_name}: {p} still exists after remove_dir({top_dir})",
            )

    def test_remove_empty_dir(self) -> None:
        dir = tempfile.mkdtemp(dir=self._root)
        self._run_checks("empty_dir", dir, {dir})

    def test_dir_with_file(self) -> None:
        dir = tempfile.mkdtemp(dir=self._root)
        _, f = tempfile.mkstemp(dir=dir)
        self._run_checks("dir_with_file", dir, {dir, f})

    def test_dir_with_symlink(self) -> None:
        dir = str(tempfile.mkdtemp(dir=self._root))
        _, f = tempfile.mkstemp(dir=dir)
        l = f"{f}_link"
        os.symlink(f, l)
        self._run_checks("dir_with_symlink", dir, {dir, f, l})

    def test_dir_with_subdir_symlink(self) -> None:
        dir = tempfile.mkdtemp(dir=self._root)
        subdir = tempfile.mkdtemp(dir=dir)
        _, f = tempfile.mkstemp(dir=subdir)
        l = f"{subdir}_link"
        os.symlink(subdir, l, target_is_directory=True)
        self._run_checks("dir_with_subdir_symlink", dir, {dir, subdir, f, l})


class GnBuildArgsTest(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = Path(self._td.name)

    def tearDown(self) -> None:
        self._td.cleanup()

    def test_find_and_read_all_build_args(self) -> None:
        # First, a ValueError is raised if the main build is missing.
        with self.assertRaises(ValueError) as cm:
            GnBuildArgs.find_and_read_all_build_args(self._root)

        self.assertEqual(
            str(cm.exception),
            f"Missing required build arguments file: {self._root}/build/bazel/gn_build_args.txt",
        )

        # Second, create a main gn_build_args.txt value, and a vendor specific one.
        main_file_path = self._root / "build/bazel/gn_build_args.txt"
        main_file_path.parent.mkdir(parents=True)
        main_file_path.write_text("foo!")

        vendor_file_path = (
            self._root / "vendor/alice&bob/build/bazel/gn_build_args.txt"
        )
        vendor_file_path.parent.mkdir(parents=True)
        vendor_file_path.write_text("BAR?")

        mapping, extra_ninja_inputs = GnBuildArgs.find_and_read_all_build_args(
            self._root
        )
        self.assertDictEqual(
            mapping,
            {
                "build/bazel/gn_build_args.txt": "foo!",
                "vendor/alice&bob/build/bazel/gn_build_args.txt": "BAR?",
            },
        )

        self.assertSetEqual(
            extra_ninja_inputs, {main_file_path, vendor_file_path}
        )

    def test_generate_args_bzl(self) -> None:
        args_json = {
            "foo": True,
            "bar": "some string",
            "zoo": False,
            "zoo2": "non-false string",
            "ignored_list": [1, 2, 3, 4],
        }

        build_args = r"""# A COMMENT TO IGNORE
# @LL@.IfChange
foo: bool
# @LL@.ThenChange(//bob.txt)

# @LL@.IfChange
bar: string
zoo: string_or_false
zoo2: string_or_false
# @LL@.ThenChange(//alice.txt)
""".replace(
            "@LL@", "LINT"
        )

        args_bzl = GnBuildArgs.generate_args_bzl(
            args_json, build_args, "path/to/build_args.txt"
        )
        self.assertEqual(
            args_bzl,
            r"""# AUTO-GENERATED BY FUCHSIA BUILD - DO NOT EDIT
# Variables listed from path/to/build_args.txt

# From //bob.txt
foo = True

# From //alice.txt
bar = "some string"
zoo = ""
zoo2 = "non-false string"

""",
        )

    def test_record_fuchsia_build_config_dir(self) -> None:
        args_json = {
            "foo": True,
            "bar": "some string",
            "zoo": False,
            "zoo2": "non-false string",
            "ignored_list": [1, 2, 3, 4],
        }

        generated = workspace_utils.GeneratedWorkspaceFiles()

        main_args_path = self._root / "build/bazel/gn_build_args.txt"
        main_args_path.parent.mkdir(parents=True)
        # NOTE: We have to use formatting to avoid the static checks on Gerrit
        # from complaining about invalid LINT statements in this file.
        main_args_path.write_text(
            r"""# A COMMENT TO IGNORE
# @LL@.IfChange
foo: bool
# @LL@.ThenChange(//main/BUILD.gn)
""".replace(
                "@LL@", "LINT"
            )
        )

        vendor_args_path = (
            self._root / "vendor/alice/build/bazel/gn_build_args.txt"
        )
        vendor_args_path.parent.mkdir(parents=True)
        vendor_args_path.write_text(
            r"""# ANOTHER COMMENT TO IGNORE
# @LL@.IfChange
bar: string
zoo: string_or_false
zoo2: string_or_false
# @LL@.ThenChange(//vendor/alice/BUILD.gn)
""".replace(
                "@LL@", "LINT"
            )
        )

        extra_ninja_inputs = GnBuildArgs.record_fuchsia_build_config_dir(
            self._root, args_json, generated
        )

        self.assertSetEqual(
            extra_ninja_inputs, {main_args_path, vendor_args_path}
        )
        generated_json = json.loads(generated.to_json())

        EXPECTED_ARGS_BZL = r"""# AUTO-GENERATED BY FUCHSIA BUILD - DO NOT EDIT
# Variables listed from build/bazel/gn_build_args.txt

# From //main/BUILD.gn
foo = True

"""

        EXPECTED_ALICE_ARGS_BZL = r"""# AUTO-GENERATED BY FUCHSIA BUILD - DO NOT EDIT
# Variables listed from vendor/alice/build/bazel/gn_build_args.txt

# From //vendor/alice/BUILD.gn
bar = "some string"
zoo = ""
zoo2 = "non-false string"

"""

        self.maxDiff = (
            None  # Ensure large dictionary differences are properly printed.
        )

        self.assertDictEqual(
            generated_json,
            {
                "BUILD.bazel": {
                    "content": "",
                    "type": "file",
                },
                "WORKSPACE.bazel": {
                    "content": "",
                    "type": "file",
                },
                "args.bzl": {
                    "content": EXPECTED_ARGS_BZL,
                    "type": "file",
                },
                "vendor_alice_args.bzl": {
                    "content": EXPECTED_ALICE_ARGS_BZL,
                    "type": "file",
                },
            },
        )


class GnTargetsDirTest(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = Path(self._td.name)

    def tearDown(self) -> None:
        self._td.cleanup()

    def test_simple(self) -> None:
        build_dir = self._root / "build"
        build_dir.mkdir()

        manifest_path = self._root / "manifest"
        manifest_path.write_text(
            json.dumps(
                [
                    {
                        "bazel_name": "package",
                        "bazel_package": "src/drivers/virtio",
                        "generator_label": "//src/drivers/virtio:package-archive(//build/toolchain/fuchsia:x64)",
                        "output_files": ["obj/src/drivers/virtio/package.far"],
                    },
                    {
                        "bazel_name": "eng.bazel_inputs",
                        "bazel_package": "bundles/assembly",
                        "generator_label": "//bundles/assembly:eng.platform_artifacts(//build/toolchain/fuchsia:x64)",
                        "output_directory": "obj/bundles/assembly/eng/platform_artifacts",
                    },
                ],
                indent=2,
            )
        )

        all_licenses_path = self._root / "all_licenses.spdx.json"
        all_licenses_path.write_text("")

        generated = workspace_utils.GeneratedWorkspaceFiles()
        workspace_utils.record_gn_targets_dir(
            generated, build_dir, manifest_path, all_licenses_path
        )

        generated_json = json.loads(generated.to_json())
        self.maxDiff = None
        self.assertListEqual(
            sorted(generated_json.keys()),
            [
                "BUILD.bazel",
                "MODULE.bazel",
                "WORKSPACE.bazel",
                "_files/obj/bundles/assembly/eng/platform_artifacts",
                "_files/obj/src/drivers/virtio/package.far",
                "all_licenses.spdx.json",
                "bundles/assembly/BUILD.bazel",
                "bundles/assembly/_files",
                "src/drivers/virtio/BUILD.bazel",
                "src/drivers/virtio/_files",
            ],
        )

        self.assertEqual(
            generated_json["BUILD.bazel"]["content"],
            r"""# AUTO-GENERATED - DO NOT EDIT
load("@rules_license//rules:license.bzl", "license")

# This contains information about all the licenses of all
# Ninja outputs exposed in this repository.
# IMPORTANT: package_name *must* be "Legacy Ninja Build Outputs"
# as several license pipeline exception files hard-code this under //vendor/...
license(
    name = "all_licenses_spdx_json",
    package_name = "Legacy Ninja Build Outputs",
    license_text = "all_licenses.spdx.json",
    visibility = ["//visibility:public"]
)

""",
        )

        self.assertEqual(
            generated_json["MODULE.bazel"]["content"],
            'module(name = "gn_targets", version = "1")\n',
        )

        self.assertEqual(
            generated_json["WORKSPACE.bazel"]["content"],
            'workspace(name = "gn_targets")\n',
        )

        self.assertDictEqual(
            generated_json["all_licenses.spdx.json"],
            {
                "target": str(all_licenses_path.resolve()),
                "type": "symlink",
            },
        )

        self.assertDictEqual(
            generated_json["_files/obj/src/drivers/virtio/package.far"],
            {
                "target": str(build_dir / "obj/src/drivers/virtio/package.far"),
                "type": "raw_symlink",
            },
        )

        self.assertDictEqual(
            generated_json[
                "_files/obj/bundles/assembly/eng/platform_artifacts"
            ],
            {
                "target": str(
                    build_dir / "obj/bundles/assembly/eng/platform_artifacts"
                ),
                "type": "raw_symlink",
            },
        )

        self.assertDictEqual(
            generated_json["src/drivers/virtio/_files"],
            {
                "target": "../../../_files",
                "type": "raw_symlink",
            },
        )

        self.assertDictEqual(
            generated_json["bundles/assembly/_files"],
            {
                "target": "../../_files",
                "type": "raw_symlink",
            },
        )

        self.assertEqual(
            generated_json["bundles/assembly/BUILD.bazel"]["content"],
            r"""# AUTO-GENERATED - DO NOT EDIT

package(
    default_applicable_licenses = ["//:all_licenses_spdx_json"],
    default_visibility = ["//visibility:public"],
)


# From GN target: //bundles/assembly:eng.platform_artifacts(//build/toolchain/fuchsia:x64)
filegroup(
    name = "eng.bazel_inputs",
    srcs = glob(["_files/obj/bundles/assembly/eng/platform_artifacts/**"], exclude_directories=1),
)
alias(
    name = "eng.bazel_inputs.directory",
    actual = "_files/obj/bundles/assembly/eng/platform_artifacts",
)
""",
        )

        self.assertEqual(
            generated_json["src/drivers/virtio/BUILD.bazel"]["content"],
            """# AUTO-GENERATED - DO NOT EDIT

package(
    default_applicable_licenses = ["//:all_licenses_spdx_json"],
    default_visibility = ["//visibility:public"],
)


# From GN target: //src/drivers/virtio:package-archive(//build/toolchain/fuchsia:x64)
filegroup(
    name = "package",
    srcs = ["_files/obj/src/drivers/virtio/package.far"],
)
""",
        )


if __name__ == "__main__":
    unittest.main()
