# Firmware Lacewing tests

This directory contains firmware Lacewing tests, which allows us to exercise the
device in fastboot mode to verify behavior.

The primary purpose here is for developer workflows that are not regularly
tested in infra, e.g. things like `getvar` variables.

Currently these tests do not run in infra and must be run manually.

## Supported devices

These tests have only been verified on these physical devices:

* NUC
* VIM3

It should be possible to get them working on other physical devices without too
much pain; the main requirements are that the device supports:

* rebooting from an SSH shell into the bootloader (i.e. `dm reboot bootloader`)
* fastboot over USB or TCP

There is currently no support for running these tests on an emulator.

## Run manually

1.  Add these args to your `fx set` command:

    ```shell
    $ fx set ... \
        --with //src/testing/sl4f \
        --with //src/sys/bin/start_sl4f \
        --with-host //src/firmware/tests/lacewing
    ```

2.  Put the device in Fuchsia mode, with SSH configured properly.

    The tests always expect the device to start fully booted, and need to be
    able to shell in remotely to reboot into fastboot mode.

3.  Run the tests:

    ```
    $ fx test //src/firmware/tests/lacewing --e2e --output
    ```
