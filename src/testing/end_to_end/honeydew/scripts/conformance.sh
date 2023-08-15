#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

uninstall_step () {
    echo "INFO: Running $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/uninstall.sh..."
    sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/uninstall.sh >/dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "INFO: uninstall.sh completed successfully"
    else
        echo
        echo "ERROR: uninstall.sh script failed."
        echo "ERROR: Please run 'sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/uninstall.sh' for further instructions"
        echo
        exit 1
    fi
}

install_step () {
    echo "INFO: Running $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/install.sh..."
    sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/install.sh >/dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "INFO: install.sh completed successfully"
    else
        echo
        echo "ERROR: install.sh script failed."
        echo "ERROR: Please run 'sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/install.sh' for further instructions"
        echo
        exit 1
    fi
}

coverage_step () {
    echo "INFO: Running $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh..."
    sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh --affected >/dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "INFO: coverage.sh completed successfully"
    else
        echo
        echo "ERROR: coverage.sh script failed."
        echo "ERROR: Please run 'sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh --affected' for further instructions"
        echo
        exit 1
    fi
}

format_step () {
    echo "INFO: Running $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/format.sh...."
    sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/format.sh >/dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "INFO: format.sh completed successfully"
    else
        echo
        echo "ERROR: format.sh script failed."
        echo "ERROR: Please run 'sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/format.sh' for further instructions"
        echo
        exit 1
    fi
}

uninstall_step
install_step
coverage_step
format_step
uninstall_step
echo
echo "INFO: Honeydew code has passed all of the conformance steps"
echo
