# Honeydew

[TOC]

Honeydew is a test framework agnostic device controller written in Python that
provides Host-(Fuchsia)Target interaction.

Supported host operating systems:
* Linux

Assumptions:
* This tool was built to be run locally. Remote workflows (i.e. where the Target
  and Host are not colocated) are in limited support, and have the following
  assumptions:
    * You use a tool like `fssh tunnel` or `funnel` to forward the Target from
      your local machine to the remote machine over a SSH tunnel
    * Only one device is currently supported over the SSH tunnel.
    * If the device reboots during the test, it may be necessary to re-run
      the `fssh tunnel` command manually again in order to re-establish the
      appropriate port forwards.
* Fastboot CLI is present on the host and is included in `$PATH` environmental
  variable (required only if you need to use [Fastboot transport]).

## Contributing

One of Honeydew's primary goals is to make it easy for anyone working on
host target interactions to contribute.

Honeydew is meant to be the one stop solution for any Host-(Fuchsia)Target
interactions. We can only make this possible when more people contribute to
Honeydew and add more and more interactions that others can also benefit.

### Getting started

* Use a Linux machine for Honeydew development and testing
* Follow [instructions on how to submit contributions to the Fuchsia project]
  for the Gerrit developer work flow

### Data dependencies

Honeydew, as packaged by the GN build system, comes bundled with some of its
data dependencies. However, if Honeydew is run outside of the build system, or
if users wish to override with custom data dependencies, the following
environment variables can be specified:
*  `HONEYDEW_FASTBOOT_OVERRIDE`: Absolute path to `fastboot` binary.

### Best Practices

Here are some of the best practices that should be followed while contributing
to Honeydew:
* Ensure there is [unit tests][unit tests] and
  [functional tests][functional tests] coverage and that you have run the
  impacted [functional tests][functional tests] (either locally or in infra) to
  make sure contributions are working.
* If a new unit test module is added,
  * ensure this new test is included in `group("tests")` section in the
    `BUILD.gn` file (located in the same directory as unit test).
  * ensure this new test is included in `group("tests")` section in the
    [top level Honeydew unit tests BUILD][top level Honeydew unit tests BUILD]
    file.
  * ensure instructions specified in [unit tests README][unit tests README]
    are sufficient to run this new test locally.
* If a new functional test module is added,
  * ensure instructions specified in
    [functional tests README][functional tests README] are sufficient to
    run this new test locally.
  * ensure this new test is included in `group("tests")` section in the
    [top level Honeydew functional tests BUILD][top level Honeydew functional tests BUILD]
    file.
  * follow
    [how to add a new test to run in infra][how to add a new test to run in infra].
* If contribution involves adding a new host target interaction method or class,
  you would need to update the corresponding interface.

### Honeydew code guidelines

There is a conformance script that users may optionally run to ensure the
Honeydew codebase's uniformity, functional correctness, and stability.

To learn more, refer to [Honeydew code guidelines](markdowns/code_guidelines.md).

**Note** - Prior to running this, please make sure to follow
[Setup](markdowns/interactive_usage.md#Setup)

**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/conformance.sh`
**successfully will ensure you have followed the guidelines. Run this script**
**and fix any errors it suggests.**

Once the script has completed successfully, it will print output similar to the
following:
```shell
INFO: Honeydew code has passed all of the conformance steps
```

## Interactive usage

If you like to use Honeydew in an interactive Python terminal refer to
[interactive usage](markdowns/interactive_usage.md).

[Best Practices]: #Best-Practices

[conformance.sh]: #honeydew-code-guidelines

[Honeydew code guidelines]: #honeydew-code-guidelines

[unit tests]: tests/unit_tests/

[unit tests README]: tests/unit_tests/README.md

[top level Honeydew unit tests BUILD]: tests/unit_tests/BUILD.gn

[functional tests]: tests/functional_tests/

[functional tests README]: tests/functional_tests/README.md

[how to add a new test to run in infra]: tests/functional_tests/README.md#How-to-add-a-new-test-to-run-in-infra

[top level Honeydew functional tests BUILD]: tests/functional_tests/BUILD.gn

[instructions on how to submit contributions to the Fuchsia project]: https://fuchsia.dev/fuchsia-src/development/source_code/contribute_changes

[Fastboot transport]: markdowns/fastboot.md
