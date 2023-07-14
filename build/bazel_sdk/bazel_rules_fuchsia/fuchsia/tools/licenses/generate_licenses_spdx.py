#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
'''Utility that produces an SPDX file containing all licenses used in a project'''

import argparse
import json
from typing import Dict, List
from sys import stderr
from fuchsia.tools.licenses.common_types import *
from fuchsia.tools.licenses.spdx_types import *


def _log(*kwargs):
    print(*kwargs, file=stderr)


def _create_doc_from_licenses_used_json(
        licenses_used_path: str, root_package_name: str,
        root_package_homepage: str, document_namespace: str,
        licenses_cross_refs_base_url: str) -> SpdxDocument:
    """
    Populates the SPDX docuemnt with bazel license rules information.

    Populates the given SPDX document with information loaded
    from a JSON dict corresponding to a
    @rules_license//rules:gather_licenses_info.bzl JSON output.
    """

    _log(f'Reading {licenses_used_path}!')
    licenses_used_json = json.load(open(licenses_used_path, 'r'))
    if isinstance(licenses_used_json, list):
        assert len(licenses_used_json) == 1
        licenses_used_dict = licenses_used_json[0]
    else:
        licenses_used_dict = licenses_used_json
    assert isinstance(licenses_used_dict, dict)
    assert 'licenses' in licenses_used_dict
    json_list = licenses_used_dict['licenses']

    # Sort the list to make the output more deterministic.
    def sort_by(d):
        return d["package_name"]

    json_list = sorted(json_list, key=sort_by)

    package_id_factory = SpdxPackageIdFactory()
    license_id_factory = SpdxLicenseIdFactory()

    packages = []
    describes = []
    relationships = []
    extracted_licenses = []

    root_package = SpdxPackage(
        spdx_id=package_id_factory.new_id(),
        name=root_package_name,
        copyright_text=None,
        license_concluded=None,
        homepage=root_package_homepage)

    packages.append(root_package)
    describes.append(root_package.spdx_id)

    for json_dict in json_list:
        dict_reader = DictReader(json_dict, location=licenses_used_path)
        bazel_package_name = dict_reader.get('package_name')
        homepage = dict_reader.get_or('package_url', None)
        copyright_notice = dict_reader.get('copyright_notice')
        license_text_file_path = dict_reader.get('license_text')

        package_id = package_id_factory.new_id()
        license_id = None
        other_doc = None
        if license_text_file_path.endswith(".spdx.json"):
            other_doc = SpdxDocument.from_json(license_text_file_path)
        else:
            with open(license_text_file_path, 'r') as text_file:
                license_id = license_id_factory.new_id()
                _log(f'Extracting {license_text_file_path}!')
                cross_ref = licenses_cross_refs_base_url + license_text_file_path
                extracted_licenses.append(
                    SpdxExtractedLicensingInfo(
                        license_id=license_id,
                        name=bazel_package_name,
                        extracted_text=text_file.read(),
                        cross_refs=[cross_ref]))

        packages.append(
            SpdxPackage(
                spdx_id=package_id,
                name=bazel_package_name,
                copyright_text=copyright_notice,
                license_concluded=SpdxLicenseExpression.create(license_id)
                if license_id else None,
                homepage=homepage,
            ))

        describes.append(package_id)
        relationships.append(
            SpdxRelationship(root_package.spdx_id, package_id, "CONTAINS"))

        if other_doc:
            other_doc = other_doc.refactor_ids(
                package_id_factory, license_id_factory)

            other_doc_index = SpdxIndex.create(other_doc)

            relationships.extend(
                [
                    SpdxRelationship(
                        package_id, root_package.spdx_id, 'CONTAINS')
                    for root_package in other_doc_index.get_root_packages()
                ])
            describes.extend(other_doc.describes)
            packages.extend(other_doc.packages)
            relationships.extend(other_doc.relationships)
            extracted_licenses.extend(other_doc.extracted_licenses)

            _log(
                f'Merged {license_text_file_path}: packages={len(other_doc.packages)} licenses={len(other_doc.extracted_licenses)}'
            )

    return SpdxDocument(
        file_path=None,
        name=root_package_name,
        namespace=document_namespace,
        creators=["Tool: fuchsia_licenses_spdx"],
        describes=describes,
        packages=packages,
        relationships=relationships,
        extracted_licenses=extracted_licenses,
    )


def _merge_duplicate_licenses(document: SpdxDocument):
    """
    Returns a copy of the document with duplicate licenses merged.

    In large projects, many prebuilts have the same dependencies.
    Dedupping the extracted licenses helps optimize the automated
    analysis and manual reviews.

    Licenses are duplicate if their name and text are the same.
    The merged licenses will inherit the union of cross references
    and see-alsos from the originals to void data loss.
    """

    license_id_factory = SpdxLicenseIdFactory()
    id_replacer = SpdxIdReplacer()
    unique_licenses: dict[str, SpdxExtractedLicensingInfo] = {}
    for license in document.extracted_licenses:
        content_id = license_id_factory.make_content_based_id(license)
        id_replacer.replace_id(old_id=license.license_id, new_id=content_id)
        if content_id not in unique_licenses:
            unique_licenses[content_id] = dataclasses.replace(
                license, license_id=content_id)
        else:
            unique_license = unique_licenses[content_id]
            unique_licenses[content_id] = unique_license.merge_with(license)

    updated_licenses = list(unique_licenses.values())
    updated_packages = [
        p.replace_license_ids(id_replacer) for p in document.packages
    ]

    return dataclasses.replace(
        document,
        packages=updated_packages,
        extracted_licenses=updated_licenses)


def main():
    '''Parses arguments.'''
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--licenses_used',
        help='JSON file containing all licenses to analyze.'
        ' The output of @rules_license//rules:gather_licenses_info.bzl',
        required=True,
    )
    parser.add_argument(
        '--spdx_output',
        help='Where to write the output spdx file.',
        required=True,
    )
    parser.add_argument(
        '--document_namespace',
        help='A unique namespace url for the SPDX references in the doc.',
        required=True,
    )
    parser.add_argument(
        '--root_package_name',
        help='The name of the root package in the SDPX doc.',
        required=True,
    )
    parser.add_argument(
        '--root_package_homepage',
        help='The homepage of the root package in the SDPX doc.',
        required=True,
    )
    parser.add_argument(
        '--licenses_cross_refs_base_url',
        help='Base URL for license paths that are local files.',
        required=True,
    )
    args = parser.parse_args()
    _log(f'Got these args {args}!')

    output_path = args.spdx_output

    document = _create_doc_from_licenses_used_json(
        licenses_used_path=args.licenses_used,
        root_package_name=args.root_package_name
        if args.root_package_name else None,
        root_package_homepage=args.root_package_homepage,
        document_namespace=args.document_namespace,
        licenses_cross_refs_base_url=args.licenses_cross_refs_base_url,
    )

    document = _merge_duplicate_licenses(document)

    _log(f'Writing {output_path}!')
    document.to_json(output_path)


if __name__ == '__main__':
    main()
