# Functional test execution in Local mode

[TOC]

## Setup
1. Ensure device type that you want to run the test on (will be listed in "device_type" field in test case's BUILD.gn file) is connected to host and detectable by FFX
```shell

$ ffx target list
NAME                SERIAL       TYPE                        STATE      ADDRS/IP                           RCS
fuchsia-emulator*   <unknown>    workstation_eng.qemu-x64    Product    [fe80::e2c:464d:6de4:4c55%qemu]    Y
```

2. Ensure the testbeds used by the test case (will be listed in "local_config_source" field in test case's BUILD.gn file) has correct device name listed

### Fuchsia Emulator
If a test case requires fuchsia emulator then follow the below steps to start it

1. Build Fuchsia with SL4F
```shell
$ jiri update -gc

$ fx set workstation_eng.qemu-x64 --with //src/testing/sl4f --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx build
```

2. Start the package server. Keep this running in the background.
```shell
$ fx serve
```

3. In a separate terminal, start the emulator
```shell
$ ffx emu stop && ffx emu start -H --net tap
```

4. Ensure shows up in FFX CLI
```shell

$ ffx target list
NAME                SERIAL       TYPE                        STATE      ADDRS/IP                           RCS
fuchsia-emulator*   <unknown>    workstation_eng.qemu-x64    Product    [fe80::e2c:464d:6de4:4c55%qemu]    Y
```

## Device class tests

### GenericFuchsiaDevice tests
```shell
$ fx set workstation_eng.qemu-x64 --with //src/testing/sl4f --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

# Start the emulator
$ ffx emu stop && ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/device_class_tests/test_generic_fuchsia_device:generic_fuchsia_device_test --e2e --output
```

### X64 tests
```shell
$ fx set workstation_eng.x64 --with //src/testing/sl4f --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/device_class_tests/test_x64:x64_test --e2e --output
```

## Affordance tests

### Component tests
```shell
$ fx set workstation_eng.qemu-x64 --with //src/testing/sl4f --with-host //src/testing/end_to_end/honeydew/tests/functional_tests:tests

# Start the emulator
$ ffx emu stop && ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/honeydew/tests/functional_tests/affordance_tests/test_component_default:component_default_test --e2e --output
```
