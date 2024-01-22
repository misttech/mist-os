# HoneyDew

[TOC]

HoneyDew is a test framework agnostic device controller written in Python that
provides Host-(Fuchsia)Target interaction.

Supported host operating systems:
* Linux

Assumptions:
* This tool was built to be run locally. Remote workflows (i.e. where the Target
  and Host are not collocated) are in limited support, and have the following
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
One of the primary goal while designing HoneyDew was to make it easy to
contribute for anyone working on Host-(Fuchsia)Target interactions.

HoneyDew is meant to be the one stop solution for any Host-(Fuchsia)Target
interactions. We can only make this possible when more people contribute to
HoneyDew and add more and more interactions that others can also benefit.

### Getting started
* HoneyDew is currently supported only on Linux. So please use a Linux machine
  for the development and testing of HoneyDew
* Follow [instructions on how to submit contributions to the Fuchsia project]
  for the Gerrit developer work flow
* If you are using [vscode IDE] then I recommend installing these
  [vscode extensions] from [vscode extension marketplace] and using these
  [vscode settings]

### Best Practices
Here are some of the best practices that should be followed while contributing
to HoneyDew:
* If contribution involves adding a new class method or new class itself, you
  may have to update the [interfaces] definitions
* Ensure there is both [unit tests] and [functional tests] coverage for
  contribution and have run the impacted [functional tests] either locally or
  in infra to make sure contribution is indeed working
* If a new unit test is added,
  * ensure this new test is included in `group("tests")` section in the
    `BUILD.gn` file (located in the same directory as unit test)
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew unit tests BUILD] file
  * ensure instructions specified in [unit tests README] are sufficient to
    run this new test locally
* If a new functional test is added,
  * ensure instructions specified in [functional tests README] are sufficient to
    run this new test locally
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew functional tests BUILD] file
  * follow [how to add a new test to run in infra]
* Ensure code is meeting all the [HoneyDew code guidelines]
* Before merging the CL, ensure CL does not introduce any regressions by
  successfully running **all** of the honeydew builders using try-jobs which
  can be found by looking for `pye2e` in the try-job name reg-ex filter field.
  While selecting these `pye2e`, do not select builders with `-subbuild` suffix.
  To know how to run try jobs refer to [sample honeydew builders using try-jobs]
* At least one of the [HoneyDew OWNERS] should be added as a reviewer
* If CL touches an existing affordance, then corresponding [Affordance OWNER]
  should be added as a reviewer
* If CL introduces a new affordance, then add yourself as [Affordance OWNER]

### HoneyDew code guidelines
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

**These guidelines need to be run at the least on the following patchsets:**
1. Initial patchset just before adding reviewers
2. Final patchset just before merging the CL
On all other patchsets, it is recommended but optional to run these guidelines.

Note - You can refer to [HoneyDew code guidelines](markdowns/code_guidelines.md)
to get insights into what are all the guidelines we are following currently

### Code Review Expectations
Here are some of the things to follow during HoneyDew CL review process as a
CL author/contributor (or) CL reviewer/approver:

#### Author
* On the initial patchset where reviewers will be added, do the following before
  starting the review:
  1. Make sure you have followed all of the [Best Practices]
  2. Include the following information in the commit message:
    ```
    ...

    Verified the following on Patchset: <initial patchset number>
    * HoneyDew code guidelines
    * Successfully ran the impacted functional tests using [LocalRun|InfraRun]
    ```
* On final patchset that will be used for merging, do the following before
  merging the CL:
  1. Re-run the [Honeydew code conformance scripts]
  2. Ensure CL does not introduce any regressions by successfully running
    **all** of the honeydew builders using try-jobs
  3. Update the commit message with the final patchset number:
    ```
    ...

    Verified the following on Patchset: <final patchset number>
    * HoneyDew code guidelines
    * Successfully ran the impacted functional tests using [LocalRun|InfraRun]
    * Successfully ran Honeydew builders using try-jobs
    ```

#### Reviewer
* Remind the CL author to follow [Best Practices] section by opening a comment
  and asking author to resolve this comment only after they verify on absolute
  final patchset that will be merged
* Verify the author has included all the information in commit message as
  mentioned [here](#Author)

## Interactive usage
If you like to use Honeydew in an interactive Python terminal refer to
[interactive usage](markdowns/interactive_usage.md).

[HoneyDew OWNERS]: ../OWNERS

[Affordance OWNER]: honeydew/interfaces/OWNERS

[Best Practices]: #Best-Practices

[HoneyDew code guidelines]: #honeydew-code-guidelines

[Honeydew code conformance scripts]: #honeydew-code-guidelines

[interfaces]: interfaces/

[unit tests]: tests/unit_tests/

[unit tests README]: tests/unit_tests/README.md

[unit tests BUILD.gn]: tests/unit_tests/BUILD.gn#10

[top level HoneyDew unit tests BUILD]: tests/unit_tests/BUILD.gn

[functional tests]: tests/functional_tests/

[functional tests README]: tests/functional_tests/README.md

[how to add a new test to run in infra]: tests/functional_tests/README.md#How-to-add-a-new-test-to-run-in-infra

[top level HoneyDew functional tests BUILD]: tests/functional_tests/BUILD.gn

[sample honeydew builders using try-jobs]: images/pye2e_builders.png

[instructions on how to submit contributions to the Fuchsia project]: https://fuchsia.dev/fuchsia-src/development/source_code/contribute_changes

[Fastboot transport]: markdowns/fastboot.md

[vscode IDE]: https://code.visualstudio.com/docs/python/python-tutorial

[vscode extension marketplace]: https://code.visualstudio.com/docs/editor/extension-marketplace

[vscode extensions]: vscode/extensions.json

[vscode settings]: vscode/settings.json

