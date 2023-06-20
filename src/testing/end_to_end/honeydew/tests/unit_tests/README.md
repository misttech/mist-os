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

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests:custom_types_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:bluetooth_gap_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:component_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:tracing_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:bluetooth_gap_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:component_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:tracing_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_classes_tests:base_fuchsia_device_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_classes_tests/sl4f:fuchsia_device_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/device_classes_tests/fuchsia_controller:fuchsia_device_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ssh_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utils_tests:http_utils_test --host --output
    ```
