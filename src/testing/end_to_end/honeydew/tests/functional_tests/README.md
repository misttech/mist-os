# Honeydew functional tests

Every Honeydew host target interaction API must have at least one functional
test case that ensures the API works as intended. For example, a functional test
checks that `<device>.reboot()` actually reboots the Fuchsia device.

[TOC]

## Which PRODUCT and BOARD to use

Running a functional test case requires a Fuchsia device. There can be many
different Fuchsia `<BOARD>`s that support multiple `<PRODUCT>` configurations.
Decide which `<PRODUCT>` and `<BOARD>` to use following this approach:
* By default, use the lowest possible `<PRODUCT>` configuration that the
corresponding functional test requires.
* For example,
  * to run `trace` affordance test cases, use `core`
  * to run `session` affordance test cases, use `terminal` (as `core` does not
    support it)

`<BOARD>`
* By default, run the test on femu unless the test can’t be verified using femu
(because femu does not yet support the functionality that corresponding
functional test needs). For example, bluetooth functional tests can’t be run on
femu.
* Else, run on NUC hardware if that test can be verified using NUC.
* Else, run on VIM3 hardware if that test can be verified using VIM3.
* Run on actual Fuchsia product only if:
  * None of the above conditions work.
  * If a specific Host-(Fuchsia)Target interaction API (say `<device>.reboot()`)
    has a special implementation for this particular product that requires
    explicit verification.
* Special cases - If a Host-(Fuchsia)Target interaction API has multiple
  implementations then ensure all the different implementations have been
  verified using whatever device type is needed.

## How to run a functional test locally

1. Check that the device type that the test runs on (will be listed in
"device_type" field in test case's BUILD.gn file) is connected to the host and
detectable by FFX:

    ```shell
    $ ffx target list
    NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
    fuchsia-emulator*   <unknown>    core.x64         Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
    ```

    Learn more about setting up these devices:
      * [femu](https://fuchsia.dev/fuchsia-src/get-started/set_up_femu)
      * [Intel NUC](https://fuchsia.dev/fuchsia-src/development/hardware/intel_nuc)
      * [vim3](https://fuchsia.dev/fuchsia-src/development/hardware/khadas-vim3)

2. Determine if your local testbed requires a manual local config. For more
information, refer to [Lacewing Mobly Config YAML file](../../../README.md#Mobly-Config-YAML-File))
**Tip**: You can most likely skip this step, if your test needs only one fuchsia
device and if your host has only that one device connected.

3. Run appropriate `fx set` command along with
   `--with //src/testing/end_to_end/honeydew/tests/functional_tests` param.
    * If test case requires `SL4f` transport, then make sure to do the following:
      * Use a `<PRODUCT>` that supports `SL4F` (such as `core`)
      * Include `--args=core_realm_shards+="[\"//src/testing/sl4f:sl4f_core_shard\"]"`
        * Alternatively, run `fx set` without the `--args` option, and then run
        `fx args`, and add the line directly to the `args.gn` file in the editor
        that opens.

4. If test needs SL4F, then run a package server in a separate terminal
    ```shell
    $ fx -d <device_name> serve
    ```

5. Run `fx test` command and specify the `target` along with `--e2e --output`
   args

  Here is an example to run a functional test on emulator
  ```shell
  $ fx set core.x64 --with //src/testing/end_to_end/honeydew/tests/functional_tests

  # start the emulator with networking enabled
  $ ffx emu stop ; ffx emu start -H --net tap

  # TODO (b/338249539): Update the path to not include 'host_x64/obj'
  $ fx test --exact host_x64/obj/src/testing/end_to_end/honeydew/tests/functional_tests/fuchsia_device_tests/test_fuchsia_device/x64_emu_test_fc.sh --e2e --output
  ```

## How to add a new test to run in infra

* Refer to [CQ VS CI vs FYI](#CQ-VS-CI-vs-FYI) section to decide which test case
build group the new test belongs to.
* Once decided, you can add it accordingly to that group by updating
[Honeydew Infra Test Groups]
* If the desired group is not present in [Honeydew Infra Test Groups], then in
  addition to creating a new group here, you may also need to:
  * Add the same group in [Lacewing Infra Test Groups] (follow the already
    available test groups in this file for reference)
  * Update the corresponding Lacewing builder configuration file (maintained by
    Foundation Infra team) to include this newly created group in
    [Lacewing Infra Test Groups]. If your test depends on target side packages,
    make sure to include the appropriate `*_packages` group. Please reach out to
    [Lacewing team] if you need help with this one. And also, please include
    [Lacewing team] as one of the reviewer in this CL.
* Update the test case's `python_mobly_test` rule in BUILD.gn to include
  appropriate BOARDS (based on what all the boards this test need to be run in
  infra) in `environments` field ([example](../../../examples/test_soft_reboot/BUILD.gn))

### CQ VS CI vs FYI

Lacewing test case can be run in Infra in of the following builder types:
* CQ builder - Builds and tests pending CLs before they get checked in, to make
  it less likely that breakages will make their way to main (and thus break the
  CI).
* CI builder - Builds and tests the code that has been checked in. Any failure
  in this builder will be reported Fuchsia Build gardeners and need to be
  triaged ASAP.
* FYI builder - Build and and tests the code that has been checked in, but
  unlike CI these builders are not expected to be green at all times.

Use the following approach in deciding whether to run the test case in CQ/CI/FYI:
* Anytime a new Lacewing test needs to be run in infra, always start in FYI.
* After 200 consecutive successful runs, test case can then be promoted to CQ/CI
  based on below criteria:
  * If infra has enough testbed capacity (that is needed to run this test) then
    it can be promoted to CQ.
  * If infra has limited testbeds (that is needed to run this test) to run this
    test then it can be promoted to CI.

Accordingly, we have created the following:
* Test groups:
  * Test group naming scheme: `<PRODUCT>_<BOARD>_<STABILITY>[ |_sl4f]_tests`
    * `<PRODUCT>` - The product that the tests require to run on - e.g. "core",
        "workbench".
    * `<BOARD>` - The board that the tests require to run on - e.g. "emulator",
        "nuc", "vim3".
    * `<STABILITY>` - Whether tests are stable or flaky - "stable" or "unstable".
        All newly added tests must be added to the "unstable" groups until 200
        passing runs in infra FYI builder have been observed, after which they
        may be promoted to "stable" groups.
    * `[ |_sl4f]` - If tests require SL4F server then include "_sl4f".
        Otherwise, leave it empty.
    * Examples:
      * `core_emulator_stable_tests`
      * `workbench_vim3_unstable_tests`
  * This naming scheme is chosen to facilitate infra builder configuration.
    * `<STABILITY>` informs whether a group is potential to run in CI/CQ
        (as it depends on `<BOARD>` also).
    * `[ |_sl4f]` informs whether a test group can be run on certain products.
    * `<BOARD>` informs whether a test group can be run on certain boards.
* Package groups:
  * Package group naming scheme: `<PRODUCT>_<BOARD>_<STABILITY>[ |_sl4f]_packages`
* Builders:
  * Package group naming scheme: `<PRODUCT>.<BOARD>-debug-pye2e-[ |staging|ci]`
    * If builder is for "CQ" then no postfix is needed but for other stages,
      postfix is necessary (`-staging` or `-ci`)
  * Examples
    * `core.x64-debug-pye2e` - CQ builder to run stable emulator and NUC tests
    * `core.x64-debug-pye2e-staging` - FYI builder to run unstable emulator and NUC tests
    * `core.vim3-debug-pye2e-staging` - FYI builder to run unstable VIM3 tests


[Honeydew Infra Test Groups]: BUILD.gn

[Lacewing Infra Test Groups]: ../../../BUILD.gn

[Lacewing team]: ../../../OWNERS
