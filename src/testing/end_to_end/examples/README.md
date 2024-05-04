# Example Mobly tests execution

[TOC]

## Setup

1. Check that the device type that the test runs on (will be listed in
"device_type" field in test case's BUILD.gn file) is connected to the host and
detectable by FFX:
    ```shell
    $ ffx target list
    NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
    fuchsia-emulator*   <unknown>    core.x64         Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
    ```
   If you need instructions to start an emulator, refer to [femu](https://fuchsia.dev/fuchsia-src/get-started/set_up_femu).

2. Determine if your local testbed requires a manual local config or not. For
more information, refer to [Lacewing Mobly Config YAML file](../README.md#Mobly-Config-YAML-File).

## Test execution in local mode

This section contains the steps to run some of the Lacewing example tests
locally

### Hello World Test

Use below commands to run HelloWorld Lacewing test locally:

```shell
$ fx set core.x64 --with //src/testing/end_to_end/examples

$ fx test //src/testing/end_to_end/examples/test_hello_world:hello_world_test_fc --e2e --output
```

### Data resource access Test

DataResourceAccess Lacewing test demonstrates accessing custom input data as
Python resource.

Use below commands to run this test locally:

```shell
$ fx set core.x64 --with //src/testing/end_to_end/examples

$ fx test //src/testing/end_to_end/examples/test_data_resource_access:data_resource_access_test_fc --e2e --output
```

### Example Revive Test Case
```shell
$ fx set workbench_eng.x64 --with //src/testing/end_to_end/examples

# To run the test class without reviving any test cases
$ fx test //src/testing/end_to_end/examples/test_case_revive_example:run_wo_test_case_revive_fc --e2e --output

# To run the test class by reviving test cases with Idle-Suspend-Auto-Resume operation
$ fx test //src/testing/end_to_end/examples/test_case_revive_example:test_case_revive_with_with_idle_suspend_auto_resume_fc --e2e --output

# To run the test class by reviving test cases with Soft-Reboot operation
$ fx test //src/testing/end_to_end/examples/test_case_revive_example:test_case_revive_with_with_soft_reboot_fc --e2e --output
```

### Soft Reboot Test

Use below commands to run SoftReboot Lacewing test locally :

```shell
# start the emulator with networking enabled
$ ffx emu stop ; ffx emu start -H --net tap

# Run SoftRebootTest using Fuchsia-Controller
$ fx set core.x64 --with //src/testing/end_to_end/examples
$ fx test //src/testing/end_to_end/examples/test_soft_reboot:soft_reboot_test_fc --e2e --output
```

### Multi Device Test

Refer to [Multi Device Test] for running multi-device Lacewing test locally.

[Multi Device Test]: test_multi_device/README.md
