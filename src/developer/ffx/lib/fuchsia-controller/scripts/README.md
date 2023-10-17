# Rapid local development environment

Use these scripts to install a development environment and rapidly
iterate on the code:

```bash
# Install a development environment.
$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python/scripts/install.sh

# Format all code for Fuchsia Controller. Run this before uploading a CL!
$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python/scripts/format.sh

# Run all tests and generate coverage. It is much faster than going through
# the whole build process, and you can output HTML using the --html-dir
# parameter.
$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python/scripts/coverage.sh --html-dir ~/fuchsia-controller-coverage

# Clean up by running uninstall.
$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python/scripts/uninstall.sh
```
