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

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/ffx:screenshot_ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/ffx:session_ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:bluetooth_avrcp_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:bluetooth_gap_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:tracing_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:user_input_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:wlan_policy_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/sl4f:wlan_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:bluetooth_avrcp_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:bluetooth_gap_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:tracing_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:wlan_policy_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/affordances_tests/fuchsia_controller:wlan_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/typing_tests:screenshot_image_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/fuchsia_device_tests:base_fuchsia_device_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/fuchsia_device_tests/sl4f:fuchsia_device_sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/fuchsia_device_tests/fuchsia_controller:fuchsia_device_fc_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:fastboot_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ffx_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:fuchsia_controller_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:ssh_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/transports_tests:sl4f_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utils_tests:common_utils_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/utils_tests:http_utils_test --host --output

    fx test //src/testing/end_to_end/honeydew/tests/unit_tests/auxiliary_devices_tests:power_switch_dmc_test --host --output
    ```
