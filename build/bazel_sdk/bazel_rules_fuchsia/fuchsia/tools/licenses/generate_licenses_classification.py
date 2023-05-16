#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
'''Utility that classifies the licenses in an SPDX file.'''

import argparse
import os
import subprocess
import sys
from fuchsia.tools.licenses.classification_types import *
from fuchsia.tools.licenses.spdx_types import *


def _log(*kwargs):
    print(*kwargs, file=sys.stderr)


def _prepare_license_files(license_files_dir: str,
                           spdx_doc: SpdxDocument) -> Dict[str, str]:
    """Extract license texts in the spdx_doc into separate files"""

    # Reuse files for duplicate license texts to speed up classification
    file_by_unique_text: Dict[str, str] = {}

    license_files_by_id = {}
    for license in spdx_doc.extracted_licenses:
        id = license.license_id
        text = license.extracted_text
        if text in file_by_unique_text:
            file_path = file_by_unique_text[text]
        else:
            file_path = os.path.join(
                license_files_dir,
                f'license{len(file_by_unique_text.keys())}.txt')
            file_by_unique_text[text] = file_path
            with open(file_path, 'w') as license_file:
                license_file.write(text)
        license_files_by_id[id] = file_path

    _log(
        f"Found {len(file_by_unique_text.keys())} unique license texts in"
        f" {len(license_files_by_id.keys())} extracted licenses.")

    return license_files_by_id


def _invoke_identify_license(
        identify_license_path: str, license_files_dir: str,
        license_files_by_id: Dict[str, str],
        default_condition: str) -> LicensesClassifications:
    """Invokes identify_license tool, returning an LicensesClassifications."""
    identify_license_output_path = 'identify_license_out.json'

    license_paths = sorted(list(set(license_files_by_id.values())))

    for path in [identify_license_path, license_files_dir] + license_paths:
        assert os.path.exists(path), f'{path} doesn\'t exist'

    _log(
        f'Producing {identify_license_output_path} using {identify_license_path}'
    )

    command = [
        identify_license_path,
        '-headers',
        f'-json={identify_license_output_path}',
        license_files_dir,
    ]

    _log(f'identify_license invocation = {command}')
    subprocess.check_output(command)

    assert os.path.exists(
        identify_license_output_path
    ), f"{identify_license_output_path} doesn't exist"

    classifications = LicensesClassifications.from_identify_license_output_json(
        identify_license_output_path, license_files_by_id, default_condition)

    _log(
        f'Found {classifications.identifications_count()} identifications for {classifications.licenses_count()} licenses'
    )

    return classifications


def _add_missing_identifications(
        spdx_doc: SpdxDocument, classifications: LicensesClassifications,
        default_condition: str) -> LicensesClassifications:
    extra_classifications = []
    for l in spdx_doc.extracted_licenses:
        if l.license_id not in classifications.license_ids():
            identification = IdentifiedSnippet.create_empty(
                l.extracted_text_lines(), condition=default_condition)
            extra_classifications.append(
                LicenseClassification(
                    license_id=l.license_id, identifications=[identification]))
    return classifications.add_classifications(extra_classifications)


def _load_override_rules(rule_paths: List[str]) -> ConditionOverrideRuleSet:
    rules = []
    for p in rule_paths:
        rule_set = ConditionOverrideRuleSet.from_json(p)
        rules.extend(rule_set.rules)
    return ConditionOverrideRuleSet(rules)


def _apply_policy_and_overrides(
    classification: LicensesClassifications,
    policy_override_rules_file_paths: List[str],
    allowed_conditions: List[str],
) -> LicensesClassifications:

    if policy_override_rules_file_paths:
        override_rules = _load_override_rules(policy_override_rules_file_paths)
        classification = classification.override_conditions(override_rules)

    classification = classification.verify_conditions(set(allowed_conditions))

    _log(
        f'{classification.failed_verifications_count()} of {classification.identifications_count()} identification failed verification'
    )

    return classification


def _print_verification_errors(
        classifications: LicensesClassifications, preamble_file_path):
    if preamble_file_path:
        with open(preamble_file_path, 'r') as preamble_file:
            preamble_text = preamble_file.read()
            _log("=====================")
            _log(preamble_text)
            _log("=====================")

    verification_messages = classifications.verification_errors()

    message_count = len(verification_messages)
    max_verification_errors = 100

    if message_count > max_verification_errors:
        verification_messages = verification_messages[0:max_verification_errors]

    for i in range(0, len(verification_messages)):
        _log(f"==========================")
        _log(f"VERIFICATION MESSAGE {i+1}/{message_count}:")
        _log(f"==========================")
        _log(verification_messages[i])

    if message_count > max_verification_errors:
        _log(
            f"WARNING: Too many verification errors. Only showing the first {max_verification_errors} of {message_count} errors."
        )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--spdx_input',
        help='An SPDX json file containing all licenses to process.'
        'The output of @fuchsia_sdk `fuchsia_licenses_spdx`',
        required=True,
    )
    parser.add_argument(
        '--identify_license_bin',
        help='Path to the identify_license binary. '
        'Expecting a binary with the same I/O as '
        'https://github.com/google/licenseidentify_license/tree/main/tools/identify_license',
        required=True,
    )
    parser.add_argument(
        '--policy_override_rules',
        help='Condition override rule files (JSON files)',
        nargs='*',
        required=True,
        default=[],
    )
    parser.add_argument(
        '--default_condition',
        help='Default condition for unmapped or unidentified licenses',
        required=False,
        default=None,
    )
    parser.add_argument(
        '--default_is_project_shipped',
        help='Default value for whether OSS projects are shipped',
        type=bool,
        required=False,
        default=False,
    )
    parser.add_argument(
        '--default_is_notice_shipped',
        help='Default value for whether OSS notice files are shipped',
        type=bool,
        required=False,
        default=False,
    )
    parser.add_argument(
        '--default_is_source_code_shipped',
        help='Default value for whether OSS source code is shipped',
        type=bool,
        required=False,
        default=False,
    )
    parser.add_argument(
        '--allowed_conditions',
        help='Conditions that are allowed',
        nargs='*',
        required=False,
        default=[],
    )

    parser.add_argument(
        '--fail_on_disallowed_conditions',
        help=
        'The tool will fail when classifications map to conditions not in the allowed list',
        type=bool,
        required=False,
        default=False,
    )

    parser.add_argument(
        '--failure_message_preamble',
        help='''Path to a text file that contains a failure message preamble.
The message will be pre-pended to the standard generated failure message,
allowing downstream customers to provide project specific instructions.
''',
        required=False,
    )

    parser.add_argument(
        '--output_file',
        help='Where to write the output json',
        required=True,
    )
    args = parser.parse_args()

    spdx_input = args.spdx_input

    _log(f'Reading license info from {spdx_input}!')
    spdx_doc = SpdxDocument.from_json(spdx_input)
    spdx_index = SpdxIndex.create(spdx_doc)

    licenses_dir = 'input_licenses'
    os.mkdir(licenses_dir)

    license_files_by_id = _prepare_license_files(licenses_dir, spdx_doc)

    classification = _invoke_identify_license(
        identify_license_path=args.identify_license_bin,
        license_files_dir=licenses_dir,
        license_files_by_id=license_files_by_id,
        default_condition=args.default_condition)

    classification = _add_missing_identifications(
        spdx_doc, classification, default_condition=args.default_condition)
    classification = classification.set_is_shipped_defaults(
        is_project_shipped=args.default_is_project_shipped,
        is_notice_shipped=args.default_is_notice_shipped,
        is_source_code_shipped=args.default_is_source_code_shipped)

    classification = classification.compute_identification_stats(spdx_index)
    classification = classification.add_licenses_information(spdx_index)
    classification = _apply_policy_and_overrides(
        classification,
        policy_override_rules_file_paths=args.policy_override_rules,
        allowed_conditions=args.allowed_conditions,
    )

    output_json_path = args.output_file
    _log(f'Writing classification into {output_json_path}!')
    classification.to_json(output_json_path)

    if args.fail_on_disallowed_conditions:
        if classification.failed_verifications_count() > 0:
            _log("ERROR: Licenses verification failed.")
            _print_verification_errors(
                classification,
                preamble_file_path=args.failure_message_preamble)
            sys.exit(-1)


if __name__ == '__main__':
    main()
