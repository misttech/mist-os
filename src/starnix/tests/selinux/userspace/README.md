## Userspace tests for SEStarnix

These tests run as userspace programs in a minimal Starnix container. The tests
can also run on Linux but the Linux version is not tested in CQ.

### Running the tests

Run on Starnix with:
```
fx test sestarnix_userspace_tests
```

Compare with the results on Linux with:
```
$FUCHSIA_DIR/src/starnix/tests/selinux/userspace/run_on_linux.py
```

### Writing a test

Add a file to the `tests/` directory, and reference it in the `test_names` list
in BUILD.gn. Don't forget to run the test on Linux before submitting!
