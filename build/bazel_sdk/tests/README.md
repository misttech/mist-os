`# Fuchsia Bazel SDK Tests

This directory contains tests meant to test the Fuchsia Bazel SDK.
These currently require to be in a Fuchsia checkout.

The test suite can be run against a local IDK build, or against the
`@fuchsia_sdk` repository created internally by the platform build
(which differ slightly in their content).

## Simple invocation

The simplest way to run them is to call `fx build bazel_sdk_tests`. This
runs the test suite against the `@fuchsia_sdk` repository. Note that
the latter only contains prebuilt binaries for the current build
configuration's `target_cpu` value.

Another way is to call `fx build bazel_sdk_tests_idk`, which runs the
suite against a locally-built IDK that contains prebuilt binaries
for all supported Fuchsia target CPU architectures. However, this
does not include a number of SDK atoms from the //sdk:platform
and //sdk:driver dependency trees.

These targets ensure that all pre-requisites are available, ensures
hermeticity and incremental build correctness. However, they will run
the full test suite every time (i.e. will not allow you to run an
individual test).

## Custom invocation

Another way is to prepare your Fuchsia build, then invoking
`scripts/bazel_test.py` manually. You can use `--test_target=<label>`
to select an individual test target (instead of all of them), or
even pass extra arguments using `-- <extra_args>`.

To run the suite against `@fuchsia_sdk`:

```
# Prepare the @fuchsia_sdk repository. Only needed once.
fx build generate_fuchsia_sdk_repository

# Run the full test suite
scripts/bazel_test.py

# Run a subset of test targets, and change test output.
scripts/bazel_test.py --test_target=:build_only_tests -- --test_output=streamed
```

While to run it against a local IDK:

```
# Prepare the IDK
fx build final_fuchsia_idk

# Run the full test suite
scripts/bazel_test.py --fuchsia_idk_directory=../../out/default/sdk/exported/fuchsia_idk

# Run a subset of test targets, and change test output.
scripts/bazel_test.py --fuchsia_idk_directory=../../out/default/sdk/exported/fuchsia_idk \
    --test_target=:build_only_tests -- --test_output=streamed
```

For more options, invoke the script with `--help`.

## Direct bazel invocation

Finally, it is possible, **on Linux only**, to directly invoke `bazel test`
in this directory after some necessary preparation, i.e. for running against
the `@fuchsia_sdk` repository:

- Run `fx build generate_fuchsia_sdk_repository` to populate pre-requisites
  from a Fuchsia checkout.

- Define the `LOCAL_FUCHSIA_PLATFORM_BUILD` to point to the build
  directory corresponding to the previous step.

- Invoke `bazel test --config=fuchsia_<cpu> <test_target>`, where
  `<cpu>` matches the CPU architecture of your Fuchsia build configuration.
  Note that without this `--config` parameter, the test suite *will* fail.

For example:

```sh
# Preparation steps (only do this once)
cd /work/fuchsia
fx set core.x64
fx build generate_fuchsia_sdk_repository
export LOCAL_FUCHSIA_PLATFORM_BUILD=$(fx get-build-dir)
cd build/bazel_sdk/tests

# Run the test suite, customize options if needed.
bazel test --config=fuchsia_x64 :tests
```

To run against a local IDK, define `LOCAL_FUCHSIA_IDK_DIRECTORY` instead
in your environment before invoking `bazel test ...` as above.

These last methods does not work on macOS because the WORKSPACE.bazel hard-codes
linux-specific paths to host prebuilt binaries (https://fxbug.dev/124321).
