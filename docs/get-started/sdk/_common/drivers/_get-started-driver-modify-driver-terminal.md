Update the source code of the sample driver and reload it to the emulator
instance.

The tasks include:

*   Update the source code of the `qemu_edu` driver.
*   Load the updated driver.
*   Run the tools component to verify the change.

Do the following:

1. Use a text editor to open the `edu_device.cc` file of the sample driver, for example:

   ```posix-terminal
   nano src/qemu_edu/drivers/edu_device.cc
   ```

1. In the `QemuEduDevice::HandleIrq` function,
   between the line `uint32_t factorial = mmio_->Read32(kFactorialComputationOffset);`
   (Line 125) and the line `FDF_SLOG(INFO, "Replying with", KV("factorial", factorial));`
   (Line 126), add the following line:

   ```
   factorial=12345;
   ```

   The function should look like below:

   ```none {:.devsite-disable-click-to-copy}
   void QemuEduDevice::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq,
                                 zx_status_t status, const zx_packet_interrupt_t* interrupt) {

   ...

     // Reply with the result.
     uint32_t factorial = mmio_->Read32(kFactorialComputationOffset);
     {{ '<strong>' }}factorial=12345;{{ '</strong>' }}
     FDF_SLOG(INFO, "Replying with", KV("factorial", factorial));
     callback(zx::ok(factorial));
   }
   ```

   The function is now updated to return the value of `12345` only.

1. Save the file and close the text editor.

1. Rebuild and run the modified sample driver:

   ```posix-terminal
   tools/bazel run //src/qemu_edu/drivers:pkg.component
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/drivers:pkg.component
   ...
   Publishing packages: [PosixPath('src/qemu_edu/drivers/qemu_edu.far')]
   Published 1 packages
   Running task: pkg.component.run_only (step 3/4)
   Registering fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm, restarting driver hosts, and attempting to bind to unbound nodes
   Successfully restarted 1 driver hosts with the driver.
   ...
   ```

1. Run the tools component using `fact 12` as input:

   ```posix-terminal
   tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ```

   This command now prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ...
   Publishing packages: [PosixPath('src/qemu_edu/tools/eductl.far')]
   Published 1 packages
   Running task: pkg.eductl_tool.run_only (step 3/4)
   {{ '<strong>' }}Factorial(12) = 12345{{ '</strong>' }}
   ...
   ```

   The output shows that the `qemu_edu` driver replied with the
   hardcoded value of `12345` to the tools component.
