# SDK Developer Workflow E2E Tests

## Overview

This directory contains a suite of tests to verify that existing developer
workflows continue to work through each new SDK release.

The workflows in question have been pulled from the OOT Driver Developer
User Guide doc [here](http://shortn/_JaGU74ejFA). Implemented test cases are in
the "tests" directory of the test workspace.

## Test Workspace

The `scripts/generate_oot_test_workspace.py` script generates a fake out-of-tree
Bazel SDK based repository, similar to repositories used by actual fuchsia
developers building drivers in stand-alone repositories.

The main differences between this test workspace and a real one are as follows:

 * The `bootstrap_bazel.sh` script in the fuchsia-infra-bazel-rules repository
   has been stubbed out. We don't want to access the internet to download bazel
   during the test. Instead, The bazel binary is copied from the fuchsia.git
   prebuilt.
 * `cipd_repository` entries in the root `WORKSPACE.bazel` file have been
   converted into `local_repository` entries. We don't want to download
   dependencies during the test. Store the new `local_repository` deps in the
   `third_party` directory. This differes slightly from OOT environments, where
   they exist in a bazel managed `external` directory.
   * `fuchsia_sdk`: symlink in the version that was locally built earlier
   in the build process.
   * `fuchsia_clang`: copy over the `third_party` project from fuchsia.git.
 * Several other files (like the CIPD manifest files in `manifest/`) are
   currently absent from the test workspace as they are not used for any of the
   existing workflow tests. They can easily be added in the future.

### Layout:

    //out/default/host_x64/gen/build/bazel_sdk/e2e/test_workspace
    ├── bazel-* // bazel managed directories, created after running bootstrap.sh
    │
    ├── BUILD.bazel
    ├── fuchsia_env.toml
    │── WORKSPACE.bazel
    │
    ├── scripts
    │   ├── bootstrap.sh -> ../third_party/fuchsia-infra-bazel-rules/scripts/bootstrap.sh
    │   ├── setup-config.sh
    │   └── update_lockfiles.sh
    │
    │ // Shac config files are created by the bootstrap script.
    ├── shac.star
    ├── shac.textproto
    │
    │ // Directory containing the tests.
    │ // Currently these files are unused, since the test is run from a GN
    │ // target in the source tree.
    │ // This is a proof of concept for executing tests like this in OOT
    │ // repositories before integrating new SDKs.
    ├── tests_dev_workflow
    │   ├── BUILD.bazel
    │   ├── ffx_sdk_version_test.py
    │   ├── ...
    │   ├── ...
    │   └── sdk_test_common.py
    │
    ├── third_party
    │   ├── fuchsia_clang
    │   │   └── ... // Copied from //prebuilt/third_party/clang
    │   ├── fuchsia-infra-bazel-rules
    │   │   └── ... // Copied from //third_party/fuchsia-infra-bazel-rules/src
    │   ├── fuchsia_sdk -> ../../../../../../../gen/build/bazel/fuchsia_sdk
    │   └── googletest
    │       └── ... // Copied from //third_party/googletest
    │
    └── tools
        ├── bazel // Copied from //prebuilt/third-party/bazel
        ├── ffx -> ../third_party/fuchsia-infra-bazel-rules/scripts/run_sdk_tool.sh
        ├── fssh -> ../third_party/fuchsia-infra-bazel-rules/scripts/run_sdk_tool.sh
        └── shac -> ../third_party/fuchsia-infra-bazel-rules/shac/scripts/shac

## Running these tests

1) Add the tests to your build graph:

    fx set <product>.<board> --with //build/bazel_sdk/e2e:tests

1) Run the tests:

    fx test
