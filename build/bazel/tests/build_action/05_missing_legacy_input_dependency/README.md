This test verifies that a GN bazel_action() target that depends on a
bazel_input_resource() that is not listed in the global list for
@legacy_ninja_build_outputs triggers an error that prints a human-friendly
error message explaining the issue and suggesting a fix.
