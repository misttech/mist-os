# Coding guidelines

[TOC]

## Current status

As of Q3 2024, Fuchsia's Python toolchain now automatically enforces most of
Honeydew's conformance requirements. Teams may optionally run `conformance.sh`
for additional verification but they are no longer required to run it before
code submission.

## Historic reasons for creating guidelines

As of Q4 2023, Fuchsia does not automatically enforce coding standards on
Python code and fix/flag the CL during development/review process.

Many teams across Fuchsia (and potentially third party vendors such as driver
development):

1. Will use Lacewing and Honeydew for the testing. It is of the utmost
   importance to ensure code works reliably.
2. Will contribute to Honeydew to add/fix Host-(Fuchsia)Target interactions.
   Some of them may not have prior Python development experience. It is of the
   utmost importance to ensure code written is consistent and meets the
   readability, quality bar set at Google and Fuchsia.

To help facilitate #1, Honeydew relies on having solid unit test and
functional test coverage.

To help facilitate #2, Honeydew relies on [Google Python Style Guide]
(which contains a list of dos and don’ts for Python programs).

## What are the guidelines?

- Ensure we have following type of tests
  - [Unit test cases](../tests/unit_tests/README.md)
    - Tests individual code units (such as functions) in isolation from the rest
      of the system by mocking all of the dependencies.
    - Makes it easy to test different error conditions, corner cases etc
    - Minimum of 70% of Honeydew code is tested using these unit tests
  - [Functional test cases](../tests/functional_tests/README.md)
    - Aims to ensure that a given API works as intended and indeed does what it
      is supposed to do (that is, `<device>.reboot()` actually reboots Fuchsia
      device) which can’t be ensured using unit test cases
    - Every single Honeydew’s Host-(Fuchsia)Target interaction API should have
      at-least one functional test case
- Ensuring code is meeting google’s Python style guide
  - Remove unused Python code using [autoflake]
  - Sort the imports using [isort]
  - Formatting using [black]
  - Linting using [pylint] (static code analysis for Python)
  - Type checking using [mypy] (static type checker for Python)

To ease the development workflow, we have
[automated checking for these guidelines](#How-to-check-for-these-guidelines?)
(everything except functional test cases). Users can run this script and
fix any errors it suggests.

## How to check for these guidelines?

**Note** - Prior to running this, please make sure to follow the
[Honeydew Setup](interactive_usage.md#Setup).

**Run** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/conformance.sh`
**and fix any errors it suggests.**

Once the script has completed successfully, it will print output similar to the
following:

```shell
INFO: Honeydew code has passed all of the conformance steps
```

This script will run the following scripts in same sequence:
1. [uninstall.sh](#un-installation)
2. [install.sh](#Installation)
3. [coverage.sh --affected](#code-coverage)
4. [format.sh](#python-style-guide)
5. [uninstall.sh](#un-installation)

### Installation

**Note** - Prior to running this, please make sure to follow the
[Honeydew Setup](interactive_usage.md#Setup).

**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/install.sh`
**will install Honeydew**

### Un-installation

**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/uninstall.sh`
**will uninstall Honeydew**

### Python Style Guide

**Run** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/format.sh`
**and fix any errors it suggests.**

### Code Coverage

**Run** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh`
**which will show comprehensive coverage on the entire Honeydew codebase.**

**For a targeted report on only the locally modified files, run the command**
**above with the `--affected` flag and fix any errors it suggests.**

[Google Python Style Guide]: https://google.github.io/styleguide/pyguide.html

[autoflake]: https://pypi.org/project/autoflake/

[isort]: https://pycqa.github.io/isort/

[pylint]: https://pypi.org/project/pylint/

[mypy]: https://mypy.readthedocs.io/en/stable/

[black]: https://github.com/psf/black

[coverage]: https://coverage.readthedocs.io/
