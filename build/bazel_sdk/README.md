# Fuchsia Bazel SDK

This directory contains the Fuchsia Bazel SDK Rules and their tests.

- bazel_rules_fuchsia: contains the rules that get merged into the final SDK
- test: contains the tests that validate the SDK

## Using a locally built SDK in your workspace

Using the Bazel SDK locally requires two steps.

1. Building the SDK locally
1. Overriding the fuchsia_sdk repository in your local checkout

### Building the SDK

The Bazel SDK that is shipped to users contains the rules that live in the
bazel_rules_fuchsia directory as well as the contents of the core IDK. There are
series of starlark rules which generate the BUILD.bazel files for the SDK. The
process of generating this final artifact is driven by the GN build system since
the core IDK is still created by GN. In order to build the bazel SDK you must
invoke a gn build via fx.

```
fx build generate_fuchsia_sdk_repository
```

Running this command will create the core SDK and run the generators which
create the Bazel SDK. This will only build for the current architecture so is
not the exact same build which is created by infrastructure but it is sufficient
enough for local development.

The output of the build can be found at `$(fx bazel info output_base)/external/fuchsia_sdk`

## Overriding the fuchsia_sdk repository in your local checkout

Once the SDK is locally built you can override your project's fuchsia_sdk repository
with the one that is built locally by using Bazel's `--override_repository`.

It can be helpful to put the path in an environment variable and create an alias
since this needs to be pass to each invocation of bazel. You can then

```
$ export FUCHSIA_SDK_PATH="$(fx bazel info output_base)/external/fuchsia_sdk"
$ export SDK_OVERRIDE="--override_repository=fuchsia_sdk=$FUCHSIA_SDK_PATH"
```

Then you can use the $SDK_OVERRIDE variable in all of your subsequent bazel
invocations

```
$ bazel build $SDK_OVERRIDE //foo:pkg
$ bazel test $SDK_OVERRIDE //foo:test
```
## Iterating on build rules

If you need to make changes to the SDK content, for example changing a fidl file,
you must recreate the Bazel SDK by running the `fx build generate_fuchsia_sdk_repository`
command. However, if you are just iterating on the starlark rules that make up
the SDK, you do not need to regenerate the SDK since these files are symlinked
into the build. You can simply make the change to the files and then trigger a
new build.

## Executing E2E Developer Workflow Tests

The `e2e` subdirectory contains a suite of tests for validating that an existing
developer workflow continues to work with the given SDK. More information on how
it works can be found in `e2e/README.md`.
