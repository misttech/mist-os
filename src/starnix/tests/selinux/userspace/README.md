## Userspace tests for SEStarnix

These tests run as userspace programs in a minimal Starnix container. The tests
can also run on Linux but the Linux version is not tested in CQ.

### Running the tests

Run on Starnix with:
```
fx test sestarnix_userspace_tests
```

For running the tests on Linux please see [//vendor/google/starnix/tests/selinux/userspace/README.md](../../../../../vendor/google/starnix/tests/selinux/userspace/README.md).


### Writing a test

Add a file to the `tests/` directory, and reference it in the `test_names` list
in BUILD.gn. Don't forget to run the test on Linux before submitting!
