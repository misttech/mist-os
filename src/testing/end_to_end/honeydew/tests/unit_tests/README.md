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

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests:bluetooth_default_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests:component_default_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests:tracing_default_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_class_tests:fuchsia_device_base_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ssh_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utility_tests:http_utils_test --host --output
    ```
