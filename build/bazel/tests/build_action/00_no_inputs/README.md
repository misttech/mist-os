This test verifies the most basic functionality of bazel_action().

- BUILD.gn contains a single GN test target definition which uses bazel_action()
  to build a Bazel target. It does not take any inputs, but expects a `foo.out`
  file to be generated in the Bazel output_base, and will copy it under the
  name `foo.ninja.out` to a Ninja-specific output location.

- BUILD.bazel contains the definition of the Bazel target that generates
  `foo.out`. It does not take any inputs, and simply writes `Hello Foo!` to
  the file.

- The `expected.gen` directory is used to verify that:

  - The generated Bazel output file is copied to the right location, and has
    the expected content (which is simply "Hello Foo!")

  - The depfile (`foo.d`) for the GN target lists the BUILD.bazel file as an
    implicit input of the corresponding GN/Ninja target.
