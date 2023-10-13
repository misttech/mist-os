#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for Collector."""


from collections import defaultdict
from gn_license_metadata import (
    GnLicenseMetadata,
    GnLicenseMetadataDB,
    GnApplicableLicensesMetadata,
)
from pathlib import Path
from typing import Dict, List, Tuple
from file_access import FileAccess
from gn_label import GnLabel
from readme_fuchsia import ReadmesDB
from collector import Collector, CollectorError, CollectorErrorKind
import unittest
import tempfile


class CollectorTest(unittest.TestCase):
    temp_dir: tempfile.TemporaryDirectory
    temp_dir_path: Path
    golibs_vendor_path: Path
    collector: Collector
    metadata_db: GnLicenseMetadataDB

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.temp_dir_path = Path(self.temp_dir.name)

        self.golibs_vendor_path = (
            self.temp_dir_path / "third_party" / "golibs" / "vendor"
        )
        self.golibs_vendor_path.mkdir(parents=True)

        file_access = FileAccess(self.temp_dir_path)
        self.metadata_db = GnLicenseMetadataDB.from_json_list([])

        self.collector = Collector(
            file_access=file_access,
            metadata_db=self.metadata_db,
            readmes_db=ReadmesDB(file_access),
        )

        return super().setUp()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()
        return super().tearDown()

    def _assert_licenses(
        self, expected_names_and_licenses: Dict[str, List[str]]
    ):
        self.collector.collect()

        # Convert expected into a set of (name, license) tuples
        expected = set()
        for name in expected_names_and_licenses.keys():
            for lic in expected_names_and_licenses[name]:
                expected.add((name, lic))
        # Convert actual into a set of (name, license) tuples
        actual = set()
        for collected_license in self.collector.unique_licenses:
            for lic in collected_license.license_files:
                actual.add((collected_license.public_name, str(lic)))
        self.maxDiff = None
        self.assertSetEqual(actual, expected)

    def _assert_errors(self, expected_error_kinds: List[CollectorErrorKind]):
        self.collector.collect()

        actual = [e.kind for e in self.collector.errors]
        self.assertSetEqual(set(actual), set(expected_error_kinds))

    def _add_license_metadata(self, target: str, name: str, files: List[str]):
        target_label = GnLabel.from_str(target)
        self.metadata_db.add_license_metadata(
            GnLicenseMetadata(
                target_label=target_label,
                public_package_name=name,
                license_files=tuple(
                    [target_label.create_child_from_str(s) for s in files]
                ),
            )
        )

    def _add_applicable_licenses_metadata(
        self,
        target: str,
        licenses: List[str],
        target_type="action",
        third_party_resources: List[str] = None,
    ):
        target_label = GnLabel.from_str(target)
        if not third_party_resources:
            third_party_resources = []
        self.metadata_db.add_applicable_licenses_metadata(
            application=GnApplicableLicensesMetadata(
                target_label=target_label,
                target_type=target_type,
                license_labels=tuple([GnLabel.from_str(s) for s in licenses]),
                third_party_resources=tuple(
                    [
                        target_label.create_child_from_str(s)
                        for s in third_party_resources
                    ]
                ),
            )
        )

    def _add_files(self, file_paths: List[str], base_path: Path = None):
        """Creates fake files in the temp dir"""
        if not base_path:
            base_path = self.temp_dir_path
        else:
            assert base_path.is_relative_to(self.temp_dir_path)
        for path in file_paths:
            path = base_path / path
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("")

    ########################### metadata tests:

    def test_target_with_metadata(self):
        self._add_license_metadata(
            target="//third_party/foo:license",
            name="Foo",
            files=["license.txt"],
        )
        self._add_applicable_licenses_metadata(
            target="//third_party/foo", licenses=["//third_party/foo:license"]
        )

        self._assert_licenses({"Foo": ["//third_party/foo/license.txt"]})

    def test_target_with_metadata_but_no_applicable_licenses(self):
        self._add_license_metadata(
            target="//third_party/foo:license",
            name="Foo",
            files=["license.txt"],
        )
        self._add_applicable_licenses_metadata(
            target="//third_party/foo", licenses=[]
        )

        self._assert_errors(
            [CollectorErrorKind.THIRD_PARTY_TARGET_WITHOUT_APPLICABLE_LICENSES]
        )

    def test_target_with_metadata_but_no_such_license_label(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/foo", licenses=["//third_party/foo:license"]
        )

        self._assert_errors(
            [CollectorErrorKind.APPLICABLE_LICENSE_REFERENCE_DOES_NOT_EXIST]
        )

    def test_non_third_party_target_without_licenses_does_not_error(self):
        self._add_applicable_licenses_metadata(target="//foo/bar", licenses=[])

        self._assert_licenses({})

    def test_non_third_party_target_with_license(self):
        self._add_license_metadata(
            target="//foo/bar:license", name="Bar", files=["license.txt"]
        )
        self._add_applicable_licenses_metadata(
            target="//foo/bar", licenses=["//foo/bar:license"]
        )

        self._assert_licenses({"Bar": ["//foo/bar/license.txt"]})

    ########################### readme tests:

    def _add_readme_file(
        self, file_path: str, name: str, license_files: List[str]
    ):
        path = self.temp_dir_path / file_path
        content = [f"NAME: {name}"]
        for l in license_files:
            content.append(f"LICENSE FILE: {l}")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("\n".join(content))

    def test_target_with_readme(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/foo", licenses=[]
        )
        self._add_files(["third_party/foo/license.txt"])
        self._add_readme_file(
            "third_party/foo/README.fuchsia",
            name="Foo",
            license_files=["license.txt"],
        )
        self._assert_licenses({"Foo": ["//third_party/foo/license.txt"]})

    def test_target_with_readme_without_license_specified(self):
        target = "//third_party/foo"
        self._add_applicable_licenses_metadata(target=target, licenses=[])
        self._add_readme_file(
            "third_party/foo/README.fuchsia", name="Foo", license_files=[]
        )
        self._assert_errors(
            [
                CollectorErrorKind.NO_LICENSE_FILE_IN_README,
                CollectorErrorKind.THIRD_PARTY_TARGET_WITHOUT_APPLICABLE_LICENSES,
            ]
        )

    def test_target_with_readme_but_license_file_not_found(self):
        target = "//third_party/foo"
        self._add_applicable_licenses_metadata(target=target, licenses=[])
        self._add_readme_file(
            "third_party/foo/README.fuchsia",
            name="Foo",
            license_files=["missing_file"],
        )
        self._assert_errors(
            [
                CollectorErrorKind.LICENSE_FILE_IN_README_NOT_FOUND,
            ]
        )

    ########################### golib tests:

    def _add_golib_vendor_files(self, file_paths: List[str]):
        self._add_files(file_paths, base_path=self.golibs_vendor_path)

    def test_golib_license_simple(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/golibs:foo/bar", licenses=[]
        )
        self._add_golib_vendor_files(["foo/bar/lib.go", "foo/bar/LICENSE"])
        self._assert_licenses(
            {"bar": ["//third_party/golibs/vendor/foo/bar/LICENSE"]}
        )

    def test_golib_license_simple(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/golibs:foo/bar", licenses=[]
        )
        self._add_golib_vendor_files(["foo/bar/lib.go", "foo/bar/LICENSE"])
        self._assert_licenses(
            {"bar": ["//third_party/golibs/vendor/foo/bar/LICENSE"]}
        )

    def test_golib_license_when_parent_dir_has_the_license(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/golibs:foo/bar", licenses=[]
        )
        self._add_golib_vendor_files(["foo/bar/lib.go", "foo/LICENSE"])
        self._assert_licenses(
            {"foo": ["//third_party/golibs/vendor/foo/LICENSE"]}
        )

    def test_golib_license_with_different_names_of_license_files(self):
        self._add_applicable_licenses_metadata(
            target="//third_party/golibs:foo", licenses=[]
        )
        self._add_golib_vendor_files(
            [
                "foo/lib.go",  # Not a license
                "foo/license",
                "foo/LICENSE-MIT",
                "foo/LICENSE.txt",
                "foo/COPYRIGHT",
                "foo/NOTICE",
                "foo/COPYING",  # Not a license
            ]
        )
        self._assert_licenses(
            {
                "foo": [
                    "//third_party/golibs/vendor/foo/license",
                    "//third_party/golibs/vendor/foo/LICENSE-MIT",
                    "//third_party/golibs/vendor/foo/LICENSE.txt",
                    "//third_party/golibs/vendor/foo/COPYRIGHT",
                    "//third_party/golibs/vendor/foo/NOTICE",
                ]
            }
        )

    def test_golib_without_license(self):
        target = "//third_party/golibs:foo"
        self._add_applicable_licenses_metadata(target=target, licenses=[])
        self._add_golib_vendor_files(["foo/lib.go"])

        self._assert_errors(
            [
                CollectorErrorKind.THIRD_PARTY_GOLIB_WITHOUT_LICENSES,
                CollectorErrorKind.THIRD_PARTY_TARGET_WITHOUT_APPLICABLE_LICENSES,
            ]
        )


if __name__ == "__main__":
    unittest.main()
