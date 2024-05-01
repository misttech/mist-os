# Fuchsia Bazel SDK

This directory contains the Fuchsia Bazel SDK Rules and their tests.

Here is what each subdir contains:

- `bazel_rules_fuchsia`: build rules that get merged into the final SDK.
- `e2e`: e2e tests that validate the SDK. See `e2e/README.md`.
- `tests`: unit tests that validate the SDK.

## Using a locally built SDK

Using a locally-built Bazel SDK requires two steps.

1. Building the SDK locally
1. Overriding the fuchsia_sdk repository in your local checkout

### Building the SDK

The Bazel SDK that is shipped to users contains the rules that live in the
bazel_rules_fuchsia directory as well as the contents of the core IDK. There are
series of starlark rules which generate the BUILD.bazel files for the SDK. The
process of generating this final artifact is driven by the GN build system since
the core IDK is still created by GN. In order to build the bazel SDK you must
invoke a gn build via fx.

```bash
$ fx build generate_fuchsia_sdk_repository
```

NOTE: By default this target will only build the Bazel SDK for the `target_cpu`.
If you need a build a full Bazel SDK locally, use `fx args` to add the following
GN argument:

```
bazel_fuchsia_sdk_all_cpus = true
```

Running this command will create the core SDK and run the generators which
create the Bazel SDK. This will only build for the current architecture so is
not the exact same build which is created by infrastructure but it is sufficient
enough for local development.

The output of the build can be found at
`$(fx bazel info output_base)/external/fuchsia_sdk`

### Overriding @fuchsia_sdk

Once the SDK is locally built you can override your project's `@fuchsia_sdk//`
repository with the one that is built locally by using Bazel's
`--override_repository`.

It can be helpful to put the path in an environment variable and create an alias
since this needs to be pass to each invocation of bazel. You can then

```bash
$ export FUCHSIA_SDK_PATH="$(fx bazel info output_base)/external/fuchsia_sdk"
$ export SDK_OVERRIDE="--override_repository=fuchsia_sdk=$FUCHSIA_SDK_PATH"
```

Then you can use the $SDK_OVERRIDE variable in all of your subsequent bazel
invocations

```bash
$ bazel build $SDK_OVERRIDE //foo:pkg
$ bazel test $SDK_OVERRIDE //foo:test
```

### Iterating on build rules

If you need to make changes to the SDK content, for example changing a fidl
file, you must recreate the Bazel SDK by running the
`fx build generate_fuchsia_sdk_repository` command. However, if you are just
iterating on the starlark rules that make up the SDK, you do not need to
regenerate the SDK since these files are symlinked into the build. You can
simply make the change to the files and then trigger a new build.

## Using an infra-built Bazel SDK in your workspace

An alternate method for testing a Bazel SDK change is to leverage the Bazel SDK
LSC process on top of a CL.
With this approach you can help remove the guesswork around local build
configuration correctness by performing the following:

1. Upload your test changes in a CL.
2. Run the LSC process. You can either:
    1. Select "Choose Tryjobs" on the gerrit page and select a builder that
       matches the following regex: `sdk-bazel-linux-.+(?<!android|subbuild)$`
    2. Run `fx lsc <cl_url> bazel_sdk`
3. Wait for the LSC builder to finish.
4. Click into the LSC builder. This will open a new builder page.
5. Click into the "run external tests" step.
6. Click into the "gerrit_link". This will open a new gerrit page.
7. Copy the contents of `patches.json` locally into your OOT repo's root.

### Iterating on build rules

If you need to make changes to the SDK content, for example changing a fidl
file, you must recreate the Bazel SDK by running the previous steps. However, if
you are just iterating on the starlark rules that make up the SDK, you can use
the following steps:

1. Follow the previous section's steps.
2. Fetch the Bazel SDK: `bazel build @fuchsia_sdk//:BUILD.bazel`
3. Make a copy of `@fuchsia_sdk`:
   `cp -r $(bazel info execution_root)/external/fuchsia_sdk/ /tmp/fuchsia_sdk`
4. Symlink builddefs:
    1. Remove existing builddefs: `rm -rf /tmp/fuchsia_sdk/{common, fuchsia}`
    2. Symlink in-tree builddefs:
        `ln -s $FUCHSIA_DIR/build/bazel_sdk/bazel_rules_fuchsia/{common, fuchsia} /tmp/fuchsia_sdk/`
5. Follow the steps in the `Overriding @fuchsia_sdk` section with
    `FUCHSIA_SDK_PATH=/tmp/fuchsia_sdk`

## Executing E2E Developer Workflow Tests

The `e2e` subdirectory contains a suite of tests for validating that an existing
developer workflow continues to work with the given SDK. More information on how
it works can be found in `e2e/README.md`.
