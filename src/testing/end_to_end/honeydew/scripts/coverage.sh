#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

# Formats the HoneyDew code as per coding guidelines

LACEWING_SRC="$FUCHSIA_DIR/src/testing/end_to_end"
HONEYDEW_SRC="$LACEWING_SRC/honeydew"
BUILD_DIR=$(cat "$FUCHSIA_DIR"/.fx-build-dir)

VENV_ROOT_PATH="$LACEWING_SRC/.venvs"
VENV_NAME="fuchsia_python_venv"
VENV_PATH="$VENV_ROOT_PATH/$VENV_NAME"

if [ -d $VENV_PATH ]
then
    echo "Activating the virtual environment..."
    source $VENV_PATH/bin/activate
else
    echo "Directory '$VENV_PATH' does not exists. Run the 'install.sh' script first..."
    exit 1
fi

echo "Configuring environment..."
OLD_PYTHONPATH=$PYTHONPATH
PYTHONPATH=$FUCHSIA_DIR/$BUILD_DIR/host_x64:$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python:$PYTHONPATH

echo "Running coverage tool..."
coverage \
    run -m unittest discover \
    --top-level-directory $HONEYDEW_SRC \
    --start-directory $HONEYDEW_SRC/tests/unit_tests \
    --pattern "*_test.py"

echo "Generating coverage stats..."
coverage report -m
rm -rf .coverage

echo "Restoring environment..."
PYTHONPATH=$OLD_PYTHONPATH
