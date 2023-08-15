# Functional test execution in Local mode

[TOC]

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
$ fx set core.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/transport_tests/test_ffx:ffx_test --e2e --output
```

[Intel NUC]: https://fuchsia.dev/fuchsia-src/development/hardware/intel_nuc

[vim3]: https://fuchsia.dev/fuchsia-src/development/hardware/khadas-vim3
