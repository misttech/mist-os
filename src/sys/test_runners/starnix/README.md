# Starnix Test Runners

Reviewed on: 2021-04-14

This directory contains two [test runners][test-runner]:

  * `starnix_test_runner`: runs test binaries that are compiled for Linux.
  * `starnix_unit_test_runner`: runs unit tests for the Starnix kernel itself.

## Starnix Test Runner

The Starnix test runner runs test binaries that are compiled for Linux. Each
such Linux binary expects to run in a particular environment (e.g., a particular
system image). Starnix calls such an environment a "galaxy."

All galaxies share the same `starnix_kernel.cml`, but they differ in their
configuration. A galaxy is simply a package that contains the Starnix kernel
component along with a system image and configuration values for its
`starnix_kernel.cml`.

The Starnix test runner expects each test component to provide its galaxy
configuration via the `program` block in the test's `.cml`.  The Starnix test
runner then instantiates the bundled Starnix kernel for each test, and uses
that runner to actually run the test binary.

This means that the same test runner can be used for all Starnix galaxies, and
each test component runs hermetically.

To create a new Starnix test component, first add the following include to the
test `.cml`:

```
include: [ "//src/proc/tests/starnix_test.shard.cml" ]
```

This shard sets the `runner` of the component to `starnix_test_runner` and
creates the collection that `starnix_test_runner` will use to instantiate the
test's `starnix_kernel` instance.

Once the `.cml` is defined, the test needs to be updated to include the
appropriate galaxy configuration values. For example,
`chromiumos_galaxy.shard.cml` contains:

```{
    program: {
        apex_hack: [],
        features: [
            "wayland",
            "custom_artifacts",
            "test_data",
        ],
        init: [],
        init_user: "root:x:0:0",
        kernel_cmdline: "",
        mounts: [
            "/:ext4:data/system.img",
            "/data:remotefs:data",
            "/dev:devtmpfs",
            "/data/tmp:tmpfs",
            "/tmp:tmpfs",
            "/dev/pts:devpts",

            // TODO(tbodt): Stop overloading /var, create a real /sys.
            "/var:sysfs",
            "/var/fs/bpf:bpf",
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