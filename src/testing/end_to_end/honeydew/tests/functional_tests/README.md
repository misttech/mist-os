# HoneyDew Functional tests

Every single HoneyDew’s Host-(Fuchsia)Target interaction API should have a
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

## CQ VS CI vs FYI
Use the following approach in deciding whether to run the test case in CQ/CI/FYI:
* Any test case that can be run using Emulator will be first run in FYI.
  After 200 consecutive successful runs, test case will be promoted to CQ.
* Any test case that can be run using hardware (NUC, VIM3 etc) will be run in
  FYI and will remain in FYI. (We are exploring options on gradually promoting
  these tests from FYI to CI but at the moment these tests will remain in FYI)

Based on this we have created the following:
* Test case build groups:
  * `emulator_tests` - Group of test cases that will be run in CQ on emulator.
  * `emulator_tests_fyi` - Group of test cases that will be run in FYI on emulator.
  * `nuc_tests_fyi` - Group of test cases that will be run in FYI on NUC.
  * `vim3_tests_fyi` - Group of test cases that will be run in FYI on VIM3.
  * `terminal_emulator_tests_fyi` - Group of test cases that will be run in FYI on emulator build using terminal.
  * format: `[Optional <PRODUCT>]_<BOARD>_tests_[ |fyi|ci]`, where
    * specifying `<PRODUCT>` is optional but recommended when tets case group is
      created for a specific `<PRODUCT>` configuration
    * if group is for "CQ" then no postfix is needed but for other stages,
      postfix is necessary (`_fyi` or `_ci`)
* Builders:
  * `core.qemu-x64-debug-pye2e` - CQ builder to run `emulator_tests` test group
  * `core.qemu-x64-debug-pye2e-staging` - FYI builder to run `emulator_tests_fyi` test group
  * `core.x64-debug-pye2e-staging` - FYI builder to run `nuc_tests_fyi` test group
  * `core.vim3-debug-pye2e-staging` - FYI builder to run `vim3_tests_fyi` test group
  * format: `<PRODUCT>.<BOARD>-debug-pye2e-[ |staging|ci]`, where
    * if builder is for "CQ" then no postfix is needed but for other stages,
      postfix is necessary (`-staging` or `-ci`)

## Setup
1. Ensure device type that you want to run the test on (will be listed in
"device_type" field in test case's BUILD.gn file) is connected to host and
detectable by FFX
    ```shell
    $ ffx target list
    NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
    fuchsia-emulator*   <unknown>    core.qemu-x64    Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
    ```

2. Ensure the testbeds used by the test case (will be listed in
"local_config_source" field in test case's BUILD.gn file) has correct device
information listed (`name`, `ssh_private_key` etc. For more information
about these fields, refer to
[Lacewing Mobly Config YAML file](../../../README.md#Mobly-Config-YAML-File))


### Fuchsia Emulator
If a test case requires fuchsia emulator then follow the below steps to start it

1. Build Fuchsia with SL4F
    ```shell
    $ jiri update -gc

    $ fx set core.qemu-x64 \
        --with //src/testing/sl4f \
        --with //src/sys/bin/start_sl4f \
        --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
        --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

    $ fx clean-build
    ```
2. In a separate terminal, start the emulator with networking enabled
    ```shell
    $ ffx emu stop ; ffx emu start -H --net tap
    ```
3. Ensure emulator shows up in FFX CLI
    ```shell
    $ ffx target list
    NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
    fuchsia-emulator*   <unknown>    core.qemu-x64    Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
    ```
4. In a separate terminal, run a package server
    ```shell
    $ fx -d <fuchsia_emulator_name> serve
    ```
5. Ensure `ffx target show` on this emulator is successful
    ```shell
    $ ffx -t <fuchsia_emulator_name> target show
    ```

### Intel NUC
If a test case requires an intel NUC or X64 hardware then

1. Follow [Intel NUC] to bring up the NUC with Fuchsia
2. Ensure NUC shows up in FFX CLI
    ```shell
    $ ffx target list
    NAME                      SERIAL       TYPE        STATE      ADDRS/IP                                       RCS
    fuchsia-54b2-038b-6e90    <unknown>    core.x64    Product    [fe80::881b:4248:1002:a7ce%enxa0cec8f442ce]    Y
    ```
3. In a separate terminal, run a package server
    ```shell
    $ fx -d <nuc_device_name> serve
    ```
4. Ensure `ffx target show` on this NUC device is successful
    ```shell
    $ ffx -t <nuc_device_name> target show
    ```

### vim3
If a test case requires an vim3 hardware then

1. Follow [vim3] to bring up the vim3 with Fuchsia
2. Ensure vim3 shows up in FFX CLI
    ```shell
    $ ffx target list
    NAME                      SERIAL       TYPE        STATE      ADDRS/IP                                       RCS
    fuchsia-c863-1470-5144    <unknown>    core.vim3   Product    [fe80::881b:4248:1002:a7ce%enxa0cec8f442ce]    Y
    ```
3. In a separate terminal, run a package server
    ```shell
    $ fx -d <vim3_device_name> serve
    ```
4. Ensure `ffx target show` on this vim3 device is successful
    ```shell
    $ ffx -t <vim3_device_name> target show
    ```

## Device class tests

### GenericFuchsiaDevice tests
```shell
$ fx set core.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

# start the emulator with networking enabled
$ ffx emu stop ; ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/device_class_tests/test_generic_fuchsia_device:generic_fuchsia_device_test --e2e --output
```

### X64 tests
```shell
$ fx set core.x64 \
    --with //build/images/recovery:recovery-installer \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/device_class_tests/test_x64:x64_test --e2e --output
```

### vim3 tests
```shell
$ fx set core.vim3 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/device_class_tests/test_vim3:vim3_test --e2e --output
```

## Affordance tests

### Bluetooth tests
``` shell
$ fx set core.x64 \
    --with //build/images/recovery:recovery-installer \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/affordance_tests/test_bluetooth:bluetooth_gap_test --e2e --output
```

### Tracing tests
``` shell
$ fx set core.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

# start the emulator with networking enabled
$ ffx emu stop ; ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/affordance_tests/test_tracing:tracing_test --e2e --output
```

### Screenshot tests

For SL4F test

```shell
$ fx set terminal.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --with //src/ui/examples:flatland-examples \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/affordance_tests/test_ui:screenshot_test --e2e --output
```

### Session tests

For SL4F test

```shell
$ fx set terminal.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --with //src/ui/examples:flatland-examples \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/affordance_tests/test_session:session_test --e2e --output
```

## Transport tests

### Fastboot tests
``` shell
$ fx set core.x64 \
    --with //build/images/recovery:recovery-installer \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/transport_tests/test_fastboot:fastboot_test --e2e --output
```

### FFX tests
``` shell
$ fx set terminal.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/transport_tests/test_ffx:ffx_test --e2e --output
```

[Intel NUC]: https://fuchsia.dev/fuchsia-src/development/hardware/intel_nuc

[vim3]: https://fuchsia.dev/fuchsia-src/development/hardware/khadas-vim3
