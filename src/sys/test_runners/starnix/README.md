# Starnix Test Runners

Reviewed on: 2021-04-14

This directory contains two [test runners][test-runner]:

  * `starnix_test_runner`: runs test binaries that are compiled for Linux.
  * `starnix_unit_test_runner`: runs unit tests for the Starnix kernel itself.

## Starnix Test Runner

The Starnix test runner runs test binaries that are compiled for Linux. Each
such Linux binary expects to run in a particular environment (e.g., a particular
system image). Starnix calls such an environment a "container."

All containers share the same `starnix_kernel.cml`, but they differ in their
configuration. A container is simply a package that contains the Starnix kernel
component along with a system image and configuration values for its
`starnix_kernel.cml`.

The Starnix test runner expects each test component to provide its container
configuration via the `program` block in the test's `.cml`.  The Starnix test
runner then instantiates the bundled Starnix kernel for each test, and uses
that runner to actually run the test binary.

This means that the same test runner can be used for all Starnix containers, and
each test component runs hermetically.

To create a new Starnix test component, first add the following include to the
test `.cml`:

```
include: [ "//src/starnix/tests/starnix_test.shard.cml" ]
```

This shard sets the `runner` of the component to `starnix_test_runner` and
creates the collection that `starnix_test_runner` will use to instantiate the
test's `starnix_kernel` instance.

Once the `.cml` is defined, the test needs to be updated to include the
appropriate container configuration values. For example,
`chromiumos_container.shard.cml` contains:

```{
    program: {
        features: [
            "wayland",
            "custom_artifacts",
            "test_data",
        ],
        init: [],
        kernel_cmdline: "",
        mounts: [
            "/:remote_bundle:data/system",
            "/data:remotefs:data",
            "/dev:devtmpfs",
            "/data/tmp:tmpfs",
            "/tmp:tmpfs",
            "/dev/pts:devpts",
            "/sys:sysfs",
            "/sys/fs/bpf:bpf",
        ],
        name: "chromiumos_test",
        startup_file_path: "",
    },
}
```

## Starnix Unit Test Runner

This runner is intended to be used by the starnix kernel's unit tests. It is a
wrapper around the regular Rust test runner that adds additional permissions
required by starnix unit tests that shouldn't be available to regular Rust test
components.

[test-runner]: ../README.md
[bionic]: https://android.googlesource.com/platform/bionic/
