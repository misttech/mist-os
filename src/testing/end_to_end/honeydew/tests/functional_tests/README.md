# Honeydew Functional tests

Every single Honeydew’s Host-(Fuchsia)Target interaction API should have a
minimum of one functional test case that aims to ensure that the API works as
intended (that is, `<device>.reboot()` actually reboots Fuchsia device)
which can’t be ensured using unit test cases.

[TOC]

## Which PRODUCT and BOARD to use
Running a functional test case requires a Fuchsia device. Since there can be
many different Fuchsia `<BOARD>`s that can support multiple `<PRODUCT>`
configurations, we will use the following approach in deciding which
`<PRODUCT>` and `<BOARD>` to use to run a given functional test:

`<PRODUCT>`
* By default use the lowest possible `<PRODUCT>` configuration that the
corresponding functional test requires.
* For example,
  * to run `trace` affordance test cases, `core` can be used
  * to run `session` affordance test cases, `terminal` need to be used (as
    `core` does not support it)

`<BOARD>`
* By default, run the test on femu unless the test can’t be verified using femu
(because femu does not yet support the functionality that corresponding
functional test needs). For example, bluetooth functional tests can’t be run on
femu
* Else, run on NUC hardware if that test can be verified using NUC
* Else, run on VIM3 hardware if that test can be verified using VIM3
* Run on actual Fuchsia product only if
  * None of the above conditions work
  * If a specific Host-(Fuchsia)Target interaction API (say `<device>.reboot()`)
    has a special implementation for this particular product that requires
    explicit verification
* Special cases - If a Host-(Fuchsia)Target interaction API has multiple
  implementations then ensure all the different implementations have been
  verified using whatever device type is needed

## How to run a functional test locally
1. Ensure device type that you want to run the test on (will be listed in
"device_type" field in test case's BUILD.gn file) is connected to host and
detectable by FFX
    ```shell
    $ ffx target list
    NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
    fuchsia-emulator*   <unknown>    core.qemu-x64    Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
    ```
  **Note**: Depending on which Fuchsia device is being used for the test, you
  can refer to following resources for more information on how to setup those
  devices:
  * [femu](https://fuchsia.dev/fuchsia-src/get-started/set_up_femu)
  * [Intel NUC](https://fuchsia.dev/fuchsia-src/development/hardware/intel_nuc)
  * [vim3](https://fuchsia.dev/fuchsia-src/development/hardware/khadas-vim3)

2. Determine if your local testbed requires a manual local config to be provided
or not. For more information, refer to
[Lacewing Mobly Config YAML file](../../../README.md#Mobly-Config-YAML-File))
**Tip**: If your test needs only one fuchsia device and if your host has only
that one device connected then you can most likely skip this step

3. Run appropriate `fx set` command along with
   `--with //src/testing/end_to_end/honeydew/tests/functional_tests` param.
    - If test case requires `SL4f` transport then make sure to use a `<PRODUCT>`
      that supports `SL4F` (such as `core`) and include
      `--args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'`

4. If test needs SL4F then run a package server in a separate terminal
    ```shell
    $ fx -d <device_name> serve
    ```

5. Run `fx test` command and specify the `target` along with `--e2e --output`
   args

  Here is an example to run a functional test on emulator
  ```shell
  $ fx set core.qemu-x64 \
      --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
      --with //src/testing/end_to_end/honeydew/tests/functional_tests

  # start the emulator with networking enabled
  $ ffx emu stop ; ffx emu start -H --net tap

  $ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/fuchsia_device_tests/test_fuchsia_device:x64_emu_test_sl4f --e2e --output

  $ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/fuchsia_device_tests/test_fuchsia_device:x64_emu_test_fc --e2e --output
  ```

## How to add a new test to run in infra
* Refer to [CQ VS CI vs FYI](#CQ-VS-CI-vs-FYI) section to decide which test case
build group the new test belongs to
* Once decided, you can add it accordingly to that group by updating
[Honeydew Infra Test Groups]
* If the desired group is not present in [Honeydew Infra Test Groups] then in
  addition to creating a new group here, you may also need to
  * Add the same group in [Lacewing Infra Test Groups] (follow the already
    available test groups in this file for reference)
  * Update the corresponding Lacewing builder configuration file (maintained by
    Foundation Infra team) to include this newly created group in
    [Lacewing Infra Test Groups]. If your test depends on target side packages,
    make sure to include the appropriate `*_packages` group. Please reach out to
    Lacewing team if you need help with this one. And also, please include
    Lacewing team as one of the reviewer in this CL.

### CQ VS CI vs FYI
Use the following approach in deciding whether to run the test case in CQ/CI/FYI:
* Any test case that can be run using Emulator will be first run in FYI.
  After 200 consecutive successful runs, test case will be promoted to CQ.
* Any test case that can be run using hardware (NUC, VIM3 etc) will be run in
  FYI and will remain in FYI. (We are exploring options on gradually promoting
  these tests from FYI to CI but at the moment these tests will remain in FYI).

Based on this we have created the following:
* Test case build groups:
  * Test group naming scheme: `<STABILITY>_<BOARD>[ |_sl4f]_tests`
    * `<STABILITY>` - Whether tests are stable or flaky - "stable" or "unstable".
        All newly added tests must be added to the "unstable" groups until 200
        passing runs in infra FYI builder have been observed, after which they
        may be promoted to "stable" groups.
    * `<BOARD>` - The board that the tests require to run on - e.g. "emulator",
        "nuc", "vim3".
    * `[ |_sl4f]` - If tests require SL4F server then include "_sl4f".
        Otherwise, leave it empty.
    * Examples:
      * `stable_emulator_tests`
      * `unstable_emulator_sl4f_tests`
  * This naming scheme is chosen to facilitate infra builder configuration.
    * `<STABILITY>` informs whether a group is potential to run in CI/CQ
        (as it depends on `<BOARD>` also).
    * `[ |_sl4f]` informs whether a test group can be run on certain products.
    * `<BOARD>` informs whether a test group can be run on certain boards.
* Builder examples:
  * `core.qemu-x64-debug-pye2e` - CQ builder to run stable emulator tests
  * `core.qemu-x64-debug-pye2e-staging` - FYI builder to run unstable emulator tests
  * `core.x64-debug-pye2e-staging` - FYI builder to run unstable NUC tests
  * `core.vim3-debug-pye2e-staging` - FYI builder to run unstable VIM3 tests
  * format: `<PRODUCT>.<BOARD>-debug-pye2e-[ |staging|ci]`, where
    * if builder is for "CQ" then no postfix is needed but for other stages,
      postfix is necessary (`-staging` or `-ci`)


[Honeydew Infra Test Groups]: BUILD.gn

[Lacewing Infra Test Groups]: ../../../BUILD.gn
