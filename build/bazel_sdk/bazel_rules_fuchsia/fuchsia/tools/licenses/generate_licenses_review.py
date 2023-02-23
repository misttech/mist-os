#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
'''Utility that produces OSS licenses compliance materials.'''

import argparse
import csv
import os
import shutil
from sys import stderr
from typing import Set, List
import zipfile
from fuchsia.tools.licenses.classification_types import *
from fuchsia.tools.licenses.spdx_types import *


def _log(*kwargs):
    print(*kwargs, file=stderr)


def _dedup(input: List[str]) -> List[str]:
    return sorted(list(set(input)))


def _write_summary(
        spdx_doc: SpdxDocument, index: SpdxIndex,
        classification: LicensesClassifications, output_path: str):
    with open(output_path, "w") as csvfile:
        writer = csv.DictWriter(
            csvfile,
            fieldnames=[
                "name",
                "identifications",
                "conditions",
                "overriden_conditions",
                "link",
                "tracking_issues",
                "comments",
                # The following 'debugging' fields begin with _ so can be easily
                # filtered/hidden/sorted once in a spreadsheet.
                "_spdx_license_id",
                "_dependents",
                "_detailed_identifications",
                "_detailed_overrides",
                "_size_bytes",
                "_size_lines",
                "_unidentified_lines",
            ])
        writer.writeheader()

        for license in spdx_doc.extracted_licenses:
            license_id = license.license_id
            dependents = [
                ">".join([p.name
                          for p in path])
                for path in index.dependency_chains_for_license(
                    license.license_id)
            ]
            links = []
            for l in license.cross_refs:
                links.append(l)
            for l in license.see_also:
                links.append(l)
            links = _dedup(links)

            identifications = []
            conditions = []
            overriden_conditions = []
            detailed_identifications = []
            detailed_overrides = []
            tracking_issues = []
            comments = []
            identification_stats = {}
            if license_id in classification.classifications_by_id:
                license_classification = classification.classifications_by_id[
                    license_id]
                overriden_conditions = []

                identification_stats = {
                    "_size_bytes":
                        license_classification.size_bytes,
                    "_size_lines":
                        license_classification.size_lines,
                    "_unidentified_lines":
                        license_classification.unidentified_lines,
                }

                for i in license_classification.identifications:
                    identifications.append(i.identified_as)
                    conditions.append(i.condition)

                    detailed_identifications.append(
                        f"{i.identified_as} at lines {i.start_line}-{i.end_line}: {i.condition}"
                    )
                    if i.overriden_conditions:
                        overriden_conditions.extend(i.overriden_conditions)
                    if i.overriding_rules:
                        for r in i.overriding_rules:
                            detailed_overrides.append(
                                f"{i.identified_as} ({i.condition}) at {i.start_line}-{i.end_line} overriden to ({r.override_condition_to}) by {r.rule_file_path}"
                            )
                            tracking_issues.append(r.bug)
                            comments.append("\n".join(r.comment))

            row = {
                # License review columns
                "name":
                    license.name,
                "link":
                    "\n".join(_dedup(links)),
                "identifications":
                    ",\n".join(_dedup(identifications)),
                "conditions":
                    ",\n".join(_dedup(conditions)),
                "overriden_conditions":
                    ",".join(_dedup(overriden_conditions)),
                "tracking_issues":
                    "\n".join(_dedup(tracking_issues)),
                "comments":
                    "\n=======\n".join(_dedup(comments)),
                # Advanced / debugging columns
                "_spdx_license_id":
                    license_id,
                "_dependents":
                    ",\n".join(_dedup(dependents)),
                "_detailed_identifications":
                    ",\n".join(_dedup(detailed_identifications)),
                "_detailed_overrides":
                    ",\n".join(_dedup(detailed_overrides)),
            }
            row.update(identification_stats)

            writer.writerow(row)


def _zip_everything(output_dir_path: str, output_zip_path: str):
    with zipfile.ZipFile(output_zip_path, mode="w") as archive:
        for root, dirs, files in os.walk(output_dir_path):
            for file in files:
                archive.write(
                    os.path.join(root, file),
                    os.path.relpath(os.path.join(root, file), output_dir_path))


def main():
    '''Parses arguments.'''
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--spdx_input',
        help='An SPDX json file containing all licenses to process.'
        ' The output of @fuchsia_sdk `fuchsia_licenses_spdx`',
        required=True,
    )
    parser.add_argument(
        '--classification_input',
        help='A json file containing the results of'
        ' @fuchsia_sdk `fuchsia_licenses_classification`',
        required=False,
    )
    parser.add_argument(
        '--output_file',
        help='Where to write the archive containing all the output files.',
        required=True,
    )
    parser.add_argument(
        '--output_dir',
        help='Where to write all the output files.',
        required=True,
    )
    args = parser.parse_args()

    _log(f'Got these args {args}!')

    spdx_input_path = args.spdx_input
    output_dir = args.output_dir
    classification_input_path = args.classification_input

    _log(f'Reading license info from {spdx_input_path}!')
    spdx_doc = SpdxDocument.from_json(spdx_input_path)
    spdx_index = SpdxIndex.create(spdx_doc)

    _log(f'Outputing all the files into {output_dir}!')

    spdx_doc.to_json(os.path.join(output_dir, "licenses.spdx.json"))

    if classification_input_path:
        shutil.copy(
            classification_input_path,
            os.path.join(output_dir, "classification.json"))
        classification = LicensesClassifications.from_json(
            classification_input_path)
    else:
        classification = LicensesClassifications.create_empty()

    extracted_licenses_dir = os.path.join(output_dir, "extracted_licenses")
    os.mkdir(extracted_licenses_dir)
    for license in spdx_doc.extracted_licenses:
        with open(os.path.join(extracted_licenses_dir,
                               f"{license.license_id}.txt"),
                  "w") as license_file:
            license_file.write(license.extracted_text)

    _write_summary(
        spdx_doc, spdx_index, classification,
        os.path.join(output_dir, "summary.csv"))

    output_file_path = args.output_file
    _log(f'Saving all the files into {output_file_path}!')
    _zip_everything(output_dir, output_file_path)


if __name__ == '__main__':
    main()
