#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script mocks https://github.com/google/licenseclassifier/tree/main/tools/identify_license
# and verifies it is invoked as expected by the 'fuchsia_licenses_classification' rule
# when applied to 'input.spdx.json'.
# It then outputs a mock json output that is processed by the rule.

echoerr() { echo "ERROR in mock_classifier.sh: $@" 1>&2; }

verify_arg () {
    local arg_title=$1
    local actual_value=$2
    local expected_value=$3
    if [ "$actual_value" != "$expected_value" ]; then
        echoerr "Expected $arg_title to be '$expected_value' but got '$actual_value'"
        exit -1
    fi
}

verify_arg "1st argument" "$1" "-headers"
verify_arg "2st argument" "$2" "-json=identify_license_out.json"
verify_arg "3st argument" "$3" "input_licenses"
verify_arg "4st argument" "$4" "b258523163_workaround.txt"

verify_file_exists () {
    local file_path=$1
    if [ ! -f "$file_path" ]; then
        echoerr "Expected $file_path doesn't exist"
        exit -1
    fi
}

# There are 3 unique license texts in input.spdx.json
verify_file_exists input_licenses/license0.txt
verify_file_exists input_licenses/license1.txt
verify_file_exists input_licenses/license2.txt
verify_file_exists b258523163_workaround.txt

write () {
    echo "$1" >> identify_license_out.json
}

# Output mock classification. Only license0 and license1 are classified.

write '['
write '    {'
write '        "Filepath": "input_licenses/license0.txt",'
write '        "Classifications": ['
write '            {'
write '                "Name": "License Kind 1",'
write '                "Confidence": 1,'
write '                "StartLine": 1,'
write '                "EndLine": 2'
write '            },'
write '            {'
write '                "Name": "License Kind 2",'
write '                "Confidence": 0.5,'
write '                "StartLine": 2,'
write '                "EndLine": 3'
write '            }'
write '        ]'
write '    },'
write '    {'
write '        "Filepath": "input_licenses/license1.txt",'
write '        "Classifications": ['
write '            {'
write '                "Name": "License Kind 3",'
write '                "Confidence": 1,'
write '                "StartLine": 2,'
write '                "EndLine": 3'
write '            }'
write '        ]'
write '    }'
write ']'
