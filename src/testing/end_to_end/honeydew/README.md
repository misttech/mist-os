# HoneyDew

[TOC]

HoneyDew is a test framework agnostic device controller written in Python that
provides Host-(Fuchsia)Target interaction.

Supported host operating systems:
* Linux

Assumptions:
* FFX CLI is present on the host and is included in `$PATH` environmental
  variable.

## Usage

### Device object creation
```python
>>> import os
>>> import logging
>>> logging.basicConfig(level=logging.INFO)

# Update `sys.path` to include HoneyDew's path before importing it
>>> import sys
>>> FUCHSIA_ROOT = os.environ.get("FUCHSIA_DIR")
>>> HONEYDEW_ROOT = f"{FUCHSIA_ROOT}/src/testing/end_to_end"
>>> sys.path.append(HONEYDEW_ROOT)
>>> import honeydew

# honeydew.create_device() will look for a specific Fuchsia device class implementation that matches the device type specified and if it finds, it returns that specific device type object, else returns GenericFuchsiaDevice object.
# In below examples,
#   * "fuchsia-54b2-038b-6e90" is a x64 device whose implementation is present in HoneyDew. Hence returning honeydew.device_classes.x64.X64 object.
#   * "fuchsia-d88c-79a3-aa1d" is Google's 1p device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.
#   * "fuchsia-emulator" is an emulator device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.

>>> fd_1p = honeydew.create_device("fuchsia-ac67-847a-2e50", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-ac67-847a-2e50'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-ac67-847a-2e50...

>>> type(fd_1p)
honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice

>>> ws = honeydew.create_device("fuchsia-54b2-038b-6e90", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Found matching device class implementation for 'fuchsia-54b2-038b-6e90' as 'X64'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-54b2-038b-6e90...

>>> type(ws)
honeydew.device_classes.x64.X64

>>> emu = honeydew.create_device("fuchsia-emulator", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-emulator'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-emulator...

>>> type(emu)
honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice
```

### Access the static properties
```python
>>> emu.name
'fuchsia-emulator'

>>> emu.device_type
'qemu-x64'

>>> emu.product_name
'default-fuchsia'

>>> emu.manufacturer
'default-manufacturer'

>>> emu.model
'default-model'

>>> emu.serial_number
```

### Access the dynamic properties
```python
>>> emu.firmware_version
'2023-02-01T17:26:40+00:00'
```

### Access the public methods
```python
>>> emu.reboot()
INFO:honeydew.device_classes.fuchsia_device_base:Rebooting fuchsia-emulator...
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to go offline...
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator is offline.
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to become pingable...
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator now pingable.
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to allow ssh connection...
Warning: Permanently added 'fe80::55da:a912:5df:ee98%qemu' (ED25519) to the list of known hosts.
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator is available via ssh.
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-emulator...
WARNING:honeydew.utils.http_utils:Send HTTP request failed with error: '<urlopen error [Errno 111] Connection refused>' on iteration 1/3

>>> emu.log_message_to_device(message="This is a test INFO message logged by HoneyDew", level=honeydew.custom_types.LEVEL.INFO)

>>> emu.snapshot(directory="/tmp/")
INFO:honeydew.device_classes.fuchsia_device_base:Snapshot file has been saved @ '/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'
'/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'
```

### Access the affordances

#### Component affordance
```python
>>> emu.component.search("wlanstack.cm")
True
```

#### Bluetooth affordance
```python
>>> fd_1p.bluetooth.request_discovery(True)
```

### Device object destruction
```python
>>> emu.close()
>>> del emu
```

## Contributing
One of the primary goal while designing HoneyDew was to make it easy to
contribute for anyone working on Host-(Fuchsia)Target interactions.

HoneyDew is meant to be the one stop solution for any Host-(Fuchsia)Target
interactions. We can only make this possible when more people contribute to
HoneyDew and add more and more interactions that others can also benefit.

Here are some of the pointers that you can use while contributing to HoneyDew:
* HoneyDew is currently supported only on Linux. So please use a Linux machine
  for the development and testing of HoneyDew
* Follow [instructions on how to submit contributions to the Fuchsia project]
  for the Gerrit developer work flow
* If you are using [vscode IDE] then I recommend installing
  [python extension], [pylint extension], [mypy extension], [isort extension]
  and update [user/workspace settings in vscode] to below:
    ```json
    "editor.rulers": [
        80
    ],
    "editor.formatOnSave": true,
    "editor.detectIndentation": false,
    "editor.tabSize": 4,
    "files.insertFinalNewline": true,
    "files.trimTrailingWhitespace": true,

    // To lint Python files using pylint
    "python.linting.pylintEnabled": true,
    "python.linting.pylintPath": "pylint",
    "python.linting.pylintArgs": [
        "--rcfile=${env:FUCHSIA_DIR}/src/testing/end_to_end/honeydew/pylintrc"
    ],
    "pylint.args": [
        "--rcfile=${env:FUCHSIA_DIR}/src/testing/end_to_end/honeydew/pylintrc"
    ]

    // To lint Python files using mypy
    "python.linting.mypyEnabled": true,
    "python.linting.mypyPath": "mypy",
    "python.linting.mypyArgs": [
        "--config-file=${env:FUCHSIA_DIR}/src/testing/end_to_end/honeydew/mypy.ini"
    ]
    "mypy.args" = [
        "--config-file=${env:FUCHSIA_DIR}/src/testing/end_to_end/honeydew/mypy.ini"
    ]

    // Whether to lint Python files when saved.
    "python.linting.lintOnSave": true,

    // To format Python files using yapf
    "python.formatting.provider": "yapf",
    "python.formatting.yapfPath": "yapf",

    // List of paths to libraries and the like that need to be imported by auto complete engine. E.g. when using Google App SDK, the paths are not in system path, hence need to be added into this list.
    "python.autoComplete.extraPaths": [
        "${env:FUCHSIA_DIR}/src/testing/end_to_end",
        "${env:FUCHSIA_DIR}/fuchsia/third_party/mobly/",
        "${env:FUCHSIA_DIR}/fuchsia/third_party/parameterized/",
    ],
    "python.analysis.extraPaths": [
        "${env:FUCHSIA_DIR}/fuchsia/src/testing/end_to_end",
        "${env:FUCHSIA_DIR}/fuchsia/third_party/mobly/",
        "${env:FUCHSIA_DIR}/fuchsia/third_party/parameterized/",
    ],
    // Index installed third party libraries and user files for language features such as auto-import, add import, workspace symbols and etc.
    "python.analysis.indexing": true,
    "python.analysis.inlayHints.functionReturnTypes": true,
    "python.analysis.inlayHints.variableTypes": true,
    "python.analysis.typeCheckingMode": "basic",
    "python.analysis.autoImportCompletions": true,
    "python.analysis.completeFunctionParens": true,
    ```
* If contribution involves adding a new class method or new class itself, you
  may have to update the [interfaces] definitions
* Ensure there is both [unit tests] and [functional tests] coverage for
  contribution
* If a new unit test is added,
  * ensure [unit tests README] has the instructions to run this new test
  * ensure this new test is included in `group("tests")` section in the
    `BUILD.gn` file (located in the same directory as unit test)
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew unit tests BUILD] file
* If a new functional test is added,
  * ensure [functional tests README] has the instructions to run this new test
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew functional tests BUILD] file
* Ensure code is [pylint], [mypy] and [pytype] compatible
  * Install `pylint`, `mypy` and `pytype` inside virtual environment or at system level
  * For `pylint`, you can either enable it in your IDE or
    run `pylint --rcfile=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/pylintrc $FUCHSIA_DIR/src/testing/end_to_end/honeydew/`
  * For `mypy`, you can either enable it in your IDE or
    run `mypy --config-file=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/mypy.ini $FUCHSIA_DIR/src/testing/end_to_end/honeydew/`
  * For `pytype`, you can run `pytype --config=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/pytype.toml $FUCHSIA_DIR/src/testing/end_to_end/honeydew/`
* At least one of the [HoneyDew OWNERS] should be added as a reviewer
  * Please run any impacted functional tests locally and share the test output
    with the CL reviewers
  * Please share `pylint`, `mypy` and `pytype` output with the CL reviewers

[HoneyDew OWNERS]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/OWNERS

[interfaces]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/interfaces/

[unit tests]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/unit_tests/

[unit tests README]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/unit_tests/README.md

[top level HoneyDew unit tests BUILD]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/unit_tests/BUILD.gn

[functional tests]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/functional_tests/

[functional tests README]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/functional_tests/README.md

[top level HoneyDew functional tests BUILD]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/honeydew/tests/functional_tests/BUILD.gn

[instructions on how to submit contributions to the Fuchsia project]: https://fuchsia.dev/fuchsia-src/development/source_code/contribute_changes

[//third_party]: https://fuchsia.googlesource.com/third_party/

[pylint]: https://pypi.org/project/pylint/

[mypy]: https://mypy.readthedocs.io/en/stable/

[pytype]: https://google.github.io/pytype/

[vscode IDE]: https://code.visualstudio.com/docs/python/python-tutorial

[python extension]: https://marketplace.visualstudio.com/items?itemName=ms-python.python

[pylint extension]: https://marketplace.visualstudio.com/items?itemName=ms-python.pylint

[mypy extension]: https://marketplace.visualstudio.com/items?itemName=ms-python.mypy-type-checker

[isort extension]: https://marketplace.visualstudio.com/items?itemName=ms-python.isort

[user/workspace settings in vscode]: https://code.visualstudio.com/docs/getstarted/settings
