# fx test (rewrite)

This directory contains the source code for a rewrite of `fx test`.

This tool is ready for early use, but many features are still missing.

For the current status, see [b/293917801](http://b/293917801) and its dependencies.

You can use the new `fx test` locally as follows:

```bash
$ export FUCHSIA_DISABLED_legacy_fxtest=1
fx test

# To revert
unset FUCHSIA_DISABLED_legacy_fxtest
```

Or for an individual run:

```bash
fx --disable=legacy_fxtest test
```

## Roadmap

Current Status: **Fishfood release (2)**.

1. Work in progress

   Choice of `fx test` implementation is controlled by the `fx`
   flag `legacy_fxtest`, which is enabled by default. Disabling the
   flag, as shown above, provides access to the new implementation.

   It is **not** feature complete and does not meet the requirements
   of Fuchsia testing.

1. **"Fishfood" release**

   The new implementation roughly has the features necessary to
   do testing for Fuchsia. It is still missing critical features,
   but we want an initial set of users trying it for more of their
   testing workflows.

1. "Dogfood" release

   The new implementation meets or exceeds feature parity with the
   old implementation, and we encourage all developers to disable
   the `legacy_fxtest` flag to use the new implementation as their
   daily driver.

1. Pre-release

   The old implementation will show a warning that `legacy_fxtest=0`
   will become the default, and they should switch.

1. Release

   `legacy_fxtest=0` becomes the default. The new implementation
   provides information on how to revert back to the old behavior
   if needed and where to file a bug.

1. Post-release cleanup

   The `legacy_fxtest` flag is removed, and the old implementation
   of `fx test` is deleted.

## Development Instructions

This tool is automatically included in your build. The rest of this
section provides a guide for accelerating development cycles on the
tool.

### Build just the tool

To build only the new implementation, run:

```
fx build host-tools/test2
```

This avoids a full build.

### Include and run tests

To test the new implementation's libraries, include
`--with //scripts/tools/fxtest/rewrite:tests` in your `fx set`.
For example:

```bash
fx set core.x64 --with //scripts/tools/fxtest/rewrite:tests

# This, ironically, does not yet work with the new implementation!
fx test --host
```

### Rapid local development environment

Use the scripts under the `scripts/` directory to install a development
environment and rapidly iterate on the code:

```bash
# Install a development environment.
$FUCHSIA_DIR/scripts/tools/fxtest/rewrite/scripts/install.sh

# Format all code for fx test. Run this before uploading a CL!
$FUCHSIA_DIR/scripts/tools/fxtest/rewrite/scripts/format.sh

# Run all tests and generate coverage. It is much faster than going through
# the whole build process, and you can output HTML using the --html-dir
# parameter.
$FUCHSIA_DIR/scripts/tools/fxtest/rewrite/scripts/coverage.sh --html-dir ~/fxtest-python-coverage

# Clean up by running uninstall.
$FUCHSIA_DIR/scripts/tools/fxtest/rewrite/scripts/uninstall.sh
```