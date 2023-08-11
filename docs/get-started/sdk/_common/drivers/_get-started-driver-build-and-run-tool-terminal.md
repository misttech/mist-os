The `qemu_edu` driver sample includes "tools" component named [`eductl_tools`][eductl_tools],
which can interact with the sample driver. Developers create these tools components for
testing and debugging drivers during development.

In this case, the `eductl_tool` component contacts the `qemu_edu` driver and passes
an integer as input. The driver (using the resource of the `edu` virtual device) computes
the factorial of the integer and returns the result to the eductl component. The component
then prints the result in the log.

The tasks include:

*   Build and run the `eductl_tool` component.
*   Verify that this tool can interact with the `qemu_edu` driver.

Do the following:

1. Run the `eductl_tool` component with `live` as input:

   ```posix-terminal
   tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- live
   ```

   Because the `eductl_tool` component isn’t built yet, the command first builds
   the component and then runs the component. The input argument `live` is passed to
   the `eductl_tool` component, which runs a simple test that checks whether the
   target driver (`qemu_edu`) is running in the system.

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- live
   ...
   Publishing packages: [PosixPath('src/qemu_edu/tools/eductl.far')]
   Published 1 packages
   Running task: pkg.eductl_tool.run_only (step 3/4)
   {{ '<strong>' }}Liveness check passed!{{ '</strong>' }}
   ...
   ```

   Verify that the line `Liveness check passed!` is printed in the output.

1. Run the `eductl_tool` component with `fact 12` as input:

   ```posix-terminal
   tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ```

   The input argument `fact 12` is passed to the `eductl_tool` component, which then
   requests the driver to compute the factorial of 12.

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ...
   Publishing packages: [PosixPath('src/qemu_edu/tools/eductl.far')]
   Published 1 packages
   Running task: pkg.eductl_tool.run_only (step 3/4)
   {{ '<strong>' }}Factorial(12) = 479001600{{ '</strong>' }}
   ...
   ```

   The output shows that the driver replied the result of
   `Factoria(12) = 479001600` to the `eductl_tool` component.

<!-- Reference links -->

[eductl_tools]: https://fuchsia.googlesource.com/sdk-samples/drivers/+/refs/heads/main/src/qemu_edu/tools
