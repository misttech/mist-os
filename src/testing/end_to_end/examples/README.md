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

2. Ensure the testbeds used by the test case (will be listed in
"local_config_source" field in test case's BUILD.gn file) has correct device
information listed (`name` and `ssh_private_key` fields. For more information
about these fields, refer to
[Lacewing Mobly Config YAML file](../README.md#Mobly-Config-YAML-File))

## Test execution in local mode
### Soft Reboot Test
```shell
$ fx set core.qemu-x64 \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]' \
    --with-host //src/testing/end_to_end/examples:tests

# start the emulator with networking enabled
$ ffx emu stop ; ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/examples/test_soft_reboot:soft_reboot_test --e2e --output
```

[Fuchsia Emulator]: ../honeydew/tests/functional_tests/README.md#Fuchsia-Emulator
