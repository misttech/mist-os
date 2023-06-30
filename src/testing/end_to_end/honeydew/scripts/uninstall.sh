#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

# Uninstall HoneyDew and deletes the virtual environment

LACEWING_SRC="$FUCHSIA_DIR/src/testing/end_to_end"
HONEYDEW_SRC="$LACEWING_SRC/honeydew"

VENV_ROOT_PATH="$LACEWING_SRC/.venvs"
VENV_NAME="fuchsia_python_venv"
VENV_PATH="$VENV_ROOT_PATH/$VENV_NAME"

# https://stackoverflow.com/questions/1871549/determine-if-python-is-running-inside-virtualenv
INSIDE_VENV=$(python3 -c 'import sys; print ("0" if (sys.base_prefix == sys.prefix) else "1")')
if [[ "$INSIDE_VENV" == "1" ]]; then
    echo "Inside a virtual environment. Run 'deactivate' once this script is finished..."
fi

if [ -d $VENV_PATH ]
then
    echo "Deleting '$VENV_PATH'..."
    rm -rf $VENV_PATH
fi

echo "Uninstallation is now completed..."
