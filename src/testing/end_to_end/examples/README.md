# Example Mobly Tests execution

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
   If you need instructions to start an emulator, refer to [Fuchsia Emulator].

2. Determine if your local testbed requires a manual local config to be provided
or not. For more information, refer to
[Lacewing Mobly Config YAML file](../README.md#Mobly-Config-YAML-File))

## Test execution in local mode

Refer to [SL4F vs Fuchsia-Controller] to learn more about how to run these
example lacewing tests using SL4F or Fuchsia-Controller transports.

### Hello World Test
```shell
$ fx set core.qemu-x64 --with //src/testing/end_to_end/examples

$ fx test //src/testing/end_to_end/examples/test_hello_world:hello_world_test_fc --e2e --output
```

### Data resource access Test
```shell
$ fx set core.qemu-x64 --with //src/testing/end_to_end/examples

$ fx test //src/testing/end_to_end/examples/test_data_resource_access:data_resource_access_test_fc --e2e --output
```

### Soft Reboot Test
```shell
# start the emulator with networking enabled
$ ffx emu stop ; ffx emu start -H --net tap

# Run SoftRebootTest using SL4F
$ fx set core.qemu-x64 \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with //src/testing/end_to_end/examples
$ fx test //src/testing/end_to_end/examples/test_soft_reboot:soft_reboot_test_sl4f --e2e --output

# Run SoftRebootTest using Fuchsia-Controller
$ fx set core.qemu-x64 --with //src/testing/end_to_end/examples
$ fx test //src/testing/end_to_end/examples/test_soft_reboot:soft_reboot_test_fc --e2e --output
```

### Multi Device Test
Refer to [Multi Device Test]

[SL4F vs Fuchsia-Controller]: ../honeydew/tests/functional_tests/README.md#SL4F-vs-Fuchsia_Controller

[Fuchsia Emulator]: ../honeydew/tests/functional_tests/README.md#Fuchsia-Emulator

[Multi Device Test]: test_multi_device/README.md
