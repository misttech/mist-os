This test verifies that a GN bazel_action() target that is missing a dependency
on a bazel_input_resource() that is also not listed in the global list for
@legacy_ninja_build_outputs triggers a Bazel error.
