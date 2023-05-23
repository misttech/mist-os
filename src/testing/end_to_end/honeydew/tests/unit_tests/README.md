# Unit test execution
* To run all the unit tests,
    ```shell
    fx set core.qemu-x64 --with-host //src/testing/end_to_end/honeydew/tests/unit_tests:tests
    fx test //src/testing/end_to_end/honeydew/tests/unit_tests --host --output
    ```
* To run individual unit tests,
    ```shell
    fx set core.qemu-x64 --with-host //src/testing/end_to_end/honeydew/tests/unit_tests:tests

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests:init_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:bluetooth_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:component_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:tracing_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_class_tests/sl4f:fuchsia_device_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ssh_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utility_tests:http_utils_test --host --output
    ```
