# Appkit System Validation Test

The directory contains system validation tests for appkit. The tests use existing example apps created to demonstrate how to use appkit API.

System validation sets up the test with following capability routing relationships:

```
        test_manager <-- root
            |  parent to
            V
        system_validation_test_realm (facet: system-validation) <-- system validation test root
            |  parent to
            V
        test component ( ex: `bouncing_box_system_validation`)
         /  parent to             \
        /                          \
        V                           V
    sample-app     <------>     tiles-session
(ex: `bouncing_box.cm`)               | parent to
        | depends on                  V
        V                       element_manager
      appkit
```

# Run existing tests

1. Build `workstation_eng_paused` product with system validation test targets.

```
fx set workstation_eng_paused.qemu-x64 --release --with //src/testing/system-validation:tests
fx build
```

2. Start the emulator and package server

```
ffx emu start
fx serve
```

3. Run test

```
fx test bouncing_box_system_validation
```
