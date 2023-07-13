# fx test (rewrite)

This directory contains the source code for `fx test2`, the new version of `fx test`.

This tool is under active development and should be considered an incomplete work in progress.
You can access the current state of the tool using `fx test2`. See below for the full migration roadmap.

## Roadmap

Current Status: **Work In Progress (1)**.

1. **Work in progress**

   The in-development rewrite of `fx test` will be available at `fx test2`.
   It is **not** feature complete and does not meet the requirements
   of Fuchsia testing.

1. "Fishfood" release

   `fx test2` roughly has the features necessary to do testing for
   Fuchsia. It may still be missing critical features, but we want
   an initial set of users trying it for more of their testing
   workflows.

1. "Dogfood" release

   `fx test2` meets or exceeds feature parity with `fx test`. We
   will encourage all developers to use `fx test2` as their daily
   driver. `fx test` will show an informational message encouraging
   users to switch.

1. Pre-release

   `fx test2` is the preferred way to test. `fx test` will display
   a warning to switch or alias `fx test=fx test2`

1. Release

   `fx test2` is the default way to test, and will be renamed to
   `fx test`. The old `fx test` will be renamed to `fx test1` to
   provide an escape hatch for unforeseen issues. `fx test2` will
   be an alias for `fx test` and will print a warning to switch to
   calling `fx test` directly.

1. Post-release cleanup

   `fx test` (previously `fx test2`) is the only way to test. `fx
   test2` and `fx test` aliases will be deleted.

## Development Instructions

This tool is automatically included in your build. The rest of this
section provides a guide for accelerating development cycles on the
tool.

### Build just the tool

To build only `fx test2`, run:

```
fx build host-tools/test2
```

This avoids a full build.

### Include and run tests

To test `fx test2`'s libraries, include
`--with //scripts/tools/fxtest/rewrite:tests` in your `fx set`.
For example:

```bash
fx set core.x64 --with //scripts/tools/fxtest/rewrite:tests
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