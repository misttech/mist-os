# Mobly Driver

This directory contains Mobly Driver which is a wrapper used for launching Mobly tests.
The Mobly Driver is responsible for mediating Mobly test execution in various execution environments (local/infra).

# Unit test execution
```shell
fx set core.qemu-x64 --with-host //src/testing/end_to_end/mobly_driver:tests
fx test host_x64/obj/src/testing/end_to_end/mobly_driver/api_mobly_test.sh --host --output
fx test host_x64/obj/src/testing/end_to_end/mobly_driver/mobly_driver_lib_test.sh --host --output
```
