#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for SpdxWriter."""


from file_access import FileAccess
from spdx_writer import SpdxWriter
from gn_label import GnLabel
import unittest


class MockFileAccess(FileAccess):
    def read_text(self, label: GnLabel) -> str:
        return f"TEXT FROM {label.path_str}"


class SpdxWriterTest(unittest.TestCase):
    writer: SpdxWriter

    def setUp(self) -> None:
        self.writer = SpdxWriter.create(
            root_package_name="root pkg",
            file_access=MockFileAccess(fuchsia_source_path="unused"),
        )

        return super().setUp()

    def test_empty_doc(self):
        self.writer.save_to_string()
        self.assertEqual(
            self.writer.save_to_string(),
            """{
    "spdxVersion": "SPDX-2.3",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": "root pkg",
    "documentNamespace": "",
    "creationInfo": {
        "creators": [
            "Tool: spdx_writer.py"
        ]
    },
    "dataLicense": "CC0-1.0",
    "documentDescribes": [
        "SPDXRef-Package-Root"
    ],
    "packages": [
        {
            "SPDXID": "SPDXRef-Package-Root",
            "name": "root pkg"
        }
    ],
    "relationships": [],
    "hasExtractedLicensingInfos": []
}""",
        )

    def test_add_licenses(self):
        self.writer.add_license(
            public_package_name="Foo Pkg",
            license_labels=(GnLabel.from_str("//foo/license"),),
            collection_hint="unit test",
        )
        self.writer.add_license(
            public_package_name="Bar Pkg",
            license_labels=(
                GnLabel.from_str("//bar/license"),
                GnLabel.from_str("//bar/license2"),
            ),
            collection_hint="unit test",
        )
        # Add again - should have no effect
        self.writer.add_license(
            public_package_name="Foo Pkg",
            license_labels=(GnLabel.from_str("//foo/license"),),
            collection_hint="unit test",
        )

        self.maxDiff = None
        self.assertEqual(
            self.writer.save_to_string(),
            """{
    "spdxVersion": "SPDX-2.3",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": "root pkg",
    "documentNamespace": "",
    "creationInfo": {
        "creators": [
            "Tool: spdx_writer.py"
        ]
    },
    "dataLicense": "CC0-1.0",
    "documentDescribes": [
        "SPDXRef-Package-Root",
        "SPDXRef-Package-bb49eee08f9370d9f63ca38858376072",
        "SPDXRef-Package-c27c4fa924d3caf687b5438132f42197"
    ],
    "packages": [
        {
            "SPDXID": "SPDXRef-Package-c27c4fa924d3caf687b5438132f42197",
            "name": "Bar Pkg",
            "licenseConcluded": "LicenseRef-30ea08f443cc7294bf3f6582ab0287b9 AND LicenseRef-252f9f95a13d8ba4201eba4ad1349365"
        },
        {
            "SPDXID": "SPDXRef-Package-bb49eee08f9370d9f63ca38858376072",
            "name": "Foo Pkg",
            "licenseConcluded": "LicenseRef-bb49eee08f9370d9f63ca38858376072"
        },
        {
            "SPDXID": "SPDXRef-Package-Root",
            "name": "root pkg"
        }
    ],
    "relationships": [
        {
            "spdxElementId": "SPDXRef-Package-Root",
            "relatedSpdxElement": "SPDXRef-Package-bb49eee08f9370d9f63ca38858376072",
            "relationshipType": "CONTAINS"
        },
        {
            "spdxElementId": "SPDXRef-Package-Root",
            "relatedSpdxElement": "SPDXRef-Package-c27c4fa924d3caf687b5438132f42197",
            "relationshipType": "CONTAINS"
        }
    ],
    "hasExtractedLicensingInfos": [
        {
            "name": "Bar Pkg",
            "licenseId": "LicenseRef-252f9f95a13d8ba4201eba4ad1349365",
            "extractedText": "TEXT FROM bar/license2",
            "crossRefs": [
                {
                    "url": "https://cs.opensource.google/fuchsia/fuchsia/+/main:bar/license2"
                }
            ],
            "_hint": "unit test"
        },
        {
            "name": "Bar Pkg",
            "licenseId": "LicenseRef-30ea08f443cc7294bf3f6582ab0287b9",
            "extractedText": "TEXT FROM bar/license",
            "crossRefs": [
                {
                    "url": "https://cs.opensource.google/fuchsia/fuchsia/+/main:bar/license"
                }
            ],
            "_hint": "unit test"
        },
        {
            "name": "Foo Pkg",
            "licenseId": "LicenseRef-bb49eee08f9370d9f63ca38858376072",
            "extractedText": "TEXT FROM foo/license",
            "crossRefs": [
                {
                    "url": "https://cs.opensource.google/fuchsia/fuchsia/+/main:foo/license"
                }
            ],
            "_hint": "unit test"
        }
    ]
}""",
        )


if __name__ == "__main__":
    unittest.main()
