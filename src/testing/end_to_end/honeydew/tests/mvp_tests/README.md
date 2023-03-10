# MVP Tests

This folder contains MVP tests that were identified to come up with requirements, design and implementation of Host-(Fuchsia)Target automation framework

## Local mode

### Soft Reboot Test

```shell
$ fx set workstation_eng.qemu-x64 --with //src/testing/sl4f --with-host //src/testing/end_to_end/honeydew/tests/mvp_tests:tests

# Start the emulator
$ ffx emu stop && ffx emu start -H --net tap

$ fx test //src/testing/end_to_end/honeydew/tests/mvp_tests/test_soft_reboot:soft_reboot_test --e2e --output
```
