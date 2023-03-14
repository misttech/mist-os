# Unit test execution

```shell
fx set core.qemu-x64 --with-host //src/testing/end_to_end/mobly_driver:tests
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests:api_mobly_test --host --output
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests:mobly_driver_lib_test --host --output
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests:driver_factory_test --host --output
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests/drivers:common_test --host --output
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests/drivers:local_driver_test --host --output
fx test //src/testing/end_to_end/mobly_driver/tests/unit_tests/drivers:infra_driver_test --host --output
```
