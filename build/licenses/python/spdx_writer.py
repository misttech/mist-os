#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates licenses SPDX from GN metadata."""

import json
import hashlib
import dataclasses
from pathlib import Path
from gn_label import GnLabel
from typing import Callable, Dict, List, Any, Tuple


@dataclasses.dataclass(frozen=False)
class SpdxWriter:
    "SPDX json file writer"

    file_reader_func: Callable[[Path], str]
    document_id: str
    root_package_id: str
    root_package_name: str

    json_document: Dict[str, Any] = dataclasses.field(default_factory=dict)
    document_describes: List["str"] = dataclasses.field(default_factory=list)
    packages: List[Dict[str, Any]] = dataclasses.field(default_factory=list)
    relationships: List[Dict[str,
                             Any]] = dataclasses.field(default_factory=list)
    extracted_licenses: List[Dict[str, Any]] = dataclasses.field(
        default_factory=list)
    license_json_by_ref: Dict[str,
                              Dict] = dataclasses.field(default_factory=dict)
    package_json_by_ids: Dict[str,
                              Dict] = dataclasses.field(default_factory=dict)

    def create(root_package_name: str, file_reader_func: Callable[[Path], str]):

        writer = SpdxWriter(
            file_reader_func=file_reader_func,
            document_id="SPDXRef-DOCUMENT",
            root_package_id='SPDXRef-Package-Root',
            root_package_name=root_package_name,
        )
        writer._init_json()
        return writer

    def _init_json(self):
        self.json_document.update(
            {
                'spdxVersion': 'SPDX-2.3',
                'SPDXID': self.document_id,
                'name': self.root_package_name,
                'documentNamespace': "",
                'creationInfo': {
                    'creators': [f'Tool: {Path(__file__).name}'],
                },
                'dataLicense': 'CC0-1.0',
                'documentDescribes': self.document_describes,
                'packages': self.packages,
                'relationships': self.relationships,
                'hasExtractedLicensingInfos': self.extracted_licenses,
            })

        self.document_describes.append(self.root_package_id)
        self.packages.append(
            {
                'SPDXID': self.root_package_id,
                'name': self.root_package_name,
            })

    def add_license(
            self, public_package_name: str, license_labels: Tuple[GnLabel],
            collection_hint: str):
        package_id = self._spdx_package_id(public_package_name, license_labels)

        if package_id in self.package_json_by_ids:
            # since the package_id is derived by the package name and license paths,
            # we can assume that if we already added this id, no need to add
            # new package or license elements.
            return

        license_refs = []
        for license_label in license_labels:
            license_ref = self._spdx_license_ref(
                public_package_name, license_label)
            license_refs.append(license_ref)

            if license_ref not in self.license_json_by_ref:
                license_text = self.file_reader_func(license_label.path)
                extracted_license = {
                    'name': public_package_name,
                    'licenseId': license_ref,
                    'extractedText': license_text,
                    'crossRefs': [{
                        'url': license_label.code_search_url(),
                    }]
                }
                self.license_json_by_ref[license_ref] = extracted_license
                self.extracted_licenses.append(extracted_license)

        package_json = {
            'SPDXID': package_id,
            'name': public_package_name,
            'licenseConcluded': " AND ".join(license_refs),
        }
        if collection_hint:
            package_json['sourceInformation'] = collection_hint
        self.package_json_by_ids[package_id] = package_json
        self.document_describes.append(package_id)
        self.packages.append(package_json)
        self.relationships.append(
            {
                'spdxElementId': self.root_package_id,
                'relatedSpdxElement': package_id,
                'relationshipType': 'CONTAINS',
            })

    def save(self, file_path: Path):
        with open(file_path, "w") as f:
            json.dump(self.json_document, f, indent=2)

    def save_to_string(self) -> str:
        return json.dumps(self.json_document, indent=2)

    def _spdx_package_id(
            self, public_package_name, license_labels: Tuple[GnLabel]) -> str:
        md5 = hashlib.md5()
        md5.update(public_package_name.strip().encode("utf-8"))
        for ll in license_labels:
            md5.update(str(ll.path).encode("utf-8"))
        digest = md5.hexdigest()
        return f"SPDXRef-Package-{digest}"

    def _spdx_license_ref(
            self, public_package_name, license_label: GnLabel) -> str:
        md5 = hashlib.md5()
        md5.update(public_package_name.strip().encode("utf-8"))
        md5.update(str(license_label.path).encode("utf-8"))
        digest = md5.hexdigest()
        return f"LicenseRef-{digest}"
