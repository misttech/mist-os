#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


# Formats the HoneyDew code as per coding guidelines
LACEWING_SRC="$FUCHSIA_DIR/src/testing/end_to_end"
HONEYDEW_SRC="$LACEWING_SRC/honeydew"

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

cd $FUCHSIA_DIR

echo "Removing unused code..."
autoflake \
    --in-place \
    --remove-unused-variables \
    --remove-all-unused-imports \
    --remove-duplicate-keys \
    --recursive \
    $HONEYDEW_SRC

echo "Formatting the code..."
# Sort the imports
isort $HONEYDEW_SRC
# Format the code (using YAPF)
fx format-code


echo "Running static type check using 'mypy'..."
mypy --config-file=$HONEYDEW_SRC/pyproject.toml $HONEYDEW_SRC > /dev/null 2>&1
if [ $? -eq 0 ]; then
    echo "Code is 'mypy' compliant"
else
    echo -e "\n"
    echo "Code is not 'mypy' compliant!"
    echo "Please run below command sequence, fix all the issues and then rerun this script"
    echo "*************************************"
    echo "$ source $VENV_PATH/bin/activate"
    echo "$ mypy --config-file=$HONEYDEW_SRC/pyproject.toml $HONEYDEW_SRC"
    echo "*************************************"
    echo -e "\n"
    exit 1
fi

echo "Running static code analysis using 'pylint'..."
pylint --rcfile=$HONEYDEW_SRC/linter/pylintrc $HONEYDEW_SRC/honeydew/ > /dev/null 2>&1 \
&& \
pylint --rcfile=$HONEYDEW_SRC/linter/pylintrc $HONEYDEW_SRC/tests/ > /dev/null 2>&1
if [ $? -eq 0 ]; then
    echo "Code is 'pylint' compliant"
else
    echo -e "\n"
    echo "Code is not 'pylint' compliant!"
    echo "Please run below command sequence, fix all the issues and then rerun this script"
    echo "*************************************"
    echo "$ source $VENV_PATH/bin/activate"
    echo "$ pylint --rcfile=$HONEYDEW_SRC/linter/pylintrc $HONEYDEW_SRC/honeydew/"
    echo "$ pylint --rcfile=$HONEYDEW_SRC/linter/pylintrc $HONEYDEW_SRC/tests/"
    echo "*************************************"
    echo -e "\n"
    exit 1
fi
