# Testing

## Quick Start

To build Zircon and run unit tests, run one of the following commands:

```posix-terminal
# Build and run x64.
fx set bringup_with_tests.x64

fx build

fx qemu

# Build and run arm64.
fx set bringup_with_tests.arm64

fx build

fx qemu
```

Once the scripts finish running, you should see the Zircon shell. To run
userspace tests, use the Zircon shell to run:

```posix-terminal
runtests --all
```

You can use `runtests --all -d` to see the list of tests available in the
system. You can also use `runtests -n <TEST_NAME>` to run one specific test.

To run in-kernel tests, use the Zircon shell to run:

```posix-terminal
k ut all
```

Fuchsia's [Get Started](/docs/get-started/README.md) page has more details about how to
use the Zircon shell and how to automatically build all supported architectures.

## Userspace Tests

The test harness, runtests, picks up and runs all of the executables from the
`/boot/test` and `/system/test` directories. If you provide a command-line
argument, such as `runtests -m widget_test`, runtests will only run the
single test requested -- in this case, `widget_test`.

## Kernel-mode Tests

The kernel contains unit tests and diagnostics, which can be run using the `k`
command. The output of the `k` command will only be shown on the
console. Depending on your configuration, this might be the serial console, or
the `debuglog` virtual terminal.

### Unit tests

Many parts of the kernel have unit tests, which report success/failure
automatically. These unit tests are built using the primitives provided by [the
kernel unit-test library](https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/kernel/lib/unittest/).
You can find these statically by searching for `UNITTEST_START_TESTCASE`.

These tests can be run from the shell with `k ut`. `k ut all` will run all tests
or you can use `k ut $TEST_NAME` to run a specific test.

### Diagnostics

Many parts of the kernel provide diagnostics, whose output requires manual
inspection. Some of these diagnostics are used to verify correctness (e.g.
[`timer_diag`](https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/kernel/tests/timer_tests.cc;l=198;drc=2031b0ccd0bb978f1c4056a7764f6810e527e156)),
while others simply stress test a part of the system (e.g.
[`timer_stress`](https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/kernel/tests/timer_tests.cc;l=198;drc=2031b0ccd0bb978f1c4056a7764f6810e527e156)).

To run a diagnostic, simply pass its name to the `k` command. For example, to
run the kernel's [builtin
benchmarks](https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/kernel/tests/benchmarks.cc),
run `k bench`. To find the full set of kernel diagnostics statically, search for
`STATIC_COMMAND`. To enumerate them dynamically, run `k help`.

Diagnostic tests are intended to be run via serial console, or with physical
access to the system. Some diagnostics may be destructive, and leave the system
in a broken state.
