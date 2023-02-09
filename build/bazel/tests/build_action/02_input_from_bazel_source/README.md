This test verifies that a GN bazel_action() target can invoke a Bazel build
command, passing an input source file with a GN bazel_input_resource() target.

- `BUILD.gn` defines the `bazel_input_resource("bazel_input")` target which
  wraps a simple source file (`foo.in.txt`) and makes it available in
  the Bazel repository @legacy_ninja_build_outputs as a filegroup() target
  named `01_input_resource_from_source_input`.

  It also defines a `bazel_action("test")` target which lists the first one
  through the `bazel_inputs` argument.

- `BUILD.bazel` defines a Bazel target that reads the input through its
  `@legacy_ninja_build_outputs` label, and simply copies it to its output
  location.

- The `expected.gen` directory is used to verify that the file is copied
  to the right location, and with the same content, and that the depfile
  for the GN test target lists both the `BUILD.bazel` and `foo.in.txt` files
  as implicit inputs.
