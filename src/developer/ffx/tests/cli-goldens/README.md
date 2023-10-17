# End to end tests

These are end to end tests for ffx.

* cli_compat is a compatibility test that checks that the current
command line arguments for all commands are compatible with a golden file set.

This test is written using the golden_file_test() GN template. If the golden files
need to be updated, the error message will provide the command to run to copy the
file from the output directory to the source directory. Alternatively, you can rebuild
setting the build arg `update_goldens=true`.
