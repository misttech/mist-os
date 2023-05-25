# Fuchsia Bazel SDK Tests

This directory contains tests meant to test the Fuchsia Bazel SDK.
These currently require to be in a Fuchsia checkout.

## Simple invocation

The simplest way to run them is to call `fx build bazel_sdk_tests`. This
ensures that all pre-requisites are available, ensures hermeticity
and incremental build correctness. However, it will run the full test suite
every time (i.e. will not allow you to run an individual test).

## Custom invocation

Another way is to prepare your Fuchsia build by calling
`fx build generate_fuchsia_sdk_repository` first, then invoking
`scripts/bazel_test.py`. You can use `--test_target=<label>` to
select an individual test target (instead of all of them), or even pass extra
arguments using `-- <extra_args>`, for example:

```sh
# Run the full test suite.
scripts/bazel_test.py

# Run a subset of test targets, and change test output.
scripts/bazel_test.py --test_target=:build_only_tests -- --test_output=streamed
```

For more options, invoke the script with `--help`.

## Direct bazel invocation

Finally, it is possible, **on Linux only**, to directly invoke `bazel test`
in this directory after some necessary preparation, i.e.:

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

Note that this will download random archives from the Internet, and store
all build artifacts under your $HOME/.cache/bazel/ directory.

This last method does not work on macOS because the WORKSPACE.bazel hard-codes
linux-specific paths to host prebuilt binaries (https://fxbug.dev/124321).
