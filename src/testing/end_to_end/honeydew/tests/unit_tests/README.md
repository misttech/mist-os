# Unit test execution
```shell
fx set core.qemu-x64 --with-host //src/testing/end_to_end/honeydew/tests/unit_tests:tests

fx test //src/testing/end_to_end/honeydew/tests/unit_tests:init_test --host --output

fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_class_tests:fuchsia_device_base_test --host --output

fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utility_tests:ffx_cli_test --host --output

fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utility_tests:host_utils_test --host --output

fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utility_tests:http_utils_test --host --output
```
