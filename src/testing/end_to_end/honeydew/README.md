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
* [Component affordance](markdowns/component.md)
* [Bluetooth affordance](markdowns/bluetooth.md)
* [Tracing affordance](markdowns/tracing.md)

### Device object destruction
```python
>>> emu.close()
>>> del emu
```

## HoneyDew code guidelines
### Python Style Guide
HoneyDew code follows [Google Python Style Guide] and it is important to ensure
any new code written continue to follow these guidelines.

At this point, we do not have an automated way (in CQ) for identifying this and
alerting the CL author prior to submitting the CL. Until then CL author need to
follow the below instructions every time HoneyDew code is changed.

#### Install dependencies
* Below guidelines requires certain dependencies that are not yet available
  in [Fuchsia third-party]. So for the time being you need to [pip] install
  each of these dependent module either inside a [python virtual environment]
  (activate virtual environment and run `pip install <dependency-module>`) or at
  system level (`pip install --user <dependency-module>`)
* List of all these dependencies can be found in `guidelines` and `test` lists
  defined in `[project.optional-dependencies]` section inside the
  [pyproject.toml](pyproject.toml) file


#### formatting
* Ensure code is formatted using [yapf]
* `fx format-code` underneath uses [yapf] for formatting the python code. So
   just run `fx format-code`

#### linting
* Ensure code is [pylint] compliant
* Verify `pylint` is properly installed by running `pylint --help`
* Run `pylint --rcfile=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/linter/pylintrc $FUCHSIA_DIR/src/testing/end_to_end/honeydew/` and fix all the issues pointed by `pylint`

#### type-checking
* Ensure code is [mypy] and [pytype] compliant
* For `mypy`,
  * Verify `mypy` is properly installed by running `mypy --help`
  * Run `mypy --config-file=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/linter/mypy.toml $FUCHSIA_DIR/src/testing/end_to_end/honeydew/` and fix all the issues pointed by `mypy`
* For `pytype`,
  * Verify `pytype` is properly installed by running `pytype --help`
  * Run `pytype --config=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/linter/pytype.toml $FUCHSIA_DIR/src/testing/end_to_end/honeydew/` and fix all the issues pointed by `pytype`

### Code Coverage
It is a hard requirement that HoneyDew code is well tested using a combination
of unit and functional tests.

Broadly this is how we can define the scope of each tests:
* Unit test cases
  * Tests individual code units (such as functions) in isolation from the rest
    of the system by mocking all of the dependencies.
  * Makes it easy to test different error conditions, corner cases etc
  * Minimum of 80% of HoneyDew code is tested using these unit tests
* Functional test cases
  * Aims to ensure that a given API works as intended and indeed does what it is
    supposed to do (that is, `<device>.reboot()` actually reboots Fuchsia
    device) which can’t be ensured using unit test cases
  * Every single HoneyDew’s Host-(Fuchsia)Target interaction API should have at
    least one functional test case

We use [coverage] tool for measuring 80% code coverage requirement of HoneyDew.

At this point, we do not have an automated way (in CQ) for identifying this and
alerting the CL author prior to submitting the CL. Until then CL author need to
follow the below instructions every time HoneyDew code is changed:

* Verify `coverage` is properly installed by running `coverage --help`
* Verify `parameterized` is properly installed by running
  `pip show parameterized`
* Run the below commands sequence and ensure code you have touched has unit test
  coverage
```shell
# For coverage to identify `honeydew` code as it is not pip installed
# CD to directory where `honeydew` folder exists
$ cd ${FUCHSIA_DIR}/src/testing/end_to_end

# Run unit tests using coverage tool which will run the tests
$ coverage run -m unittest discover --top-level-directory ~/fuchsia/src/testing/end_to_end/honeydew --start-directory ~/fuchsia/src/testing/end_to_end/honeydew/tests/unit_tests --pattern "*_test.py"

# Run below command to generate code coverage stats
$ coverage report -m
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
* If you are using [vscode IDE] then I recommend installing these
  [vscode extensions] from [vscode extension marketplace] and using these
  [vscode settings]
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
* Ensure code is meeting all the
  [HoneyDew code guidelines](#honeydew-code-guidelines) by sharing a proof
  (output) with CL reviewers (until CQ/Pre-Submit is updated to automatically
  check this)
* Please run any impacted functional tests locally and share the test output
with the CL reviewers
* At least one of the [HoneyDew OWNERS] should be added as a reviewer
* CL reviewers to make sure CL author has shared all the necessary output (as
  mentioned above) that was run using the latest patchset before approving the
  CL

[HoneyDew OWNERS]: ../OWNERS

[interfaces]: interfaces/

[unit tests]: tests/unit_tests/

[unit tests README]: tests/unit_tests/README.md

[unit tests BUILD.gn]: tests/unit_tests/BUILD.gn#10

[top level HoneyDew unit tests BUILD]: tests/unit_tests/BUILD.gn

[functional tests]: tests/functional_tests/

[functional tests README]: tests/functional_tests/README.md

[top level HoneyDew functional tests BUILD]: tests/functional_tests/BUILD.gn

[instructions on how to submit contributions to the Fuchsia project]: https://fuchsia.dev/fuchsia-src/development/source_code/contribute_changes

[//third_party]: https://fuchsia.googlesource.com/third_party/

[pylint]: https://pypi.org/project/pylint/

[mypy]: https://mypy.readthedocs.io/en/stable/

[pytype]: https://google.github.io/pytype/

[yapf]: https://github.com/google/yapf

[vscode IDE]: https://code.visualstudio.com/docs/python/python-tutorial

[vscode extension marketplace]: https://code.visualstudio.com/docs/editor/extension-marketplace

[vscode extensions]: vscode/extensions.json

[vscode settings]: vscode/settings.json

[Google Python Style Guide]: https://google.github.io/styleguide/pyguide.html

[Fuchsia third-party]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/third_party/

[python virtual environment]: https://docs.python.org/3/tutorial/venv.html

[coverage]: https://coverage.readthedocs.io/

[pip]: https://pip.pypa.io/en/stable/getting-started/
