Use the `ffx debug` command to step through the sample driver’s code while the
driver is running on the emulator instance.

The tasks include:

*   Identify the driver host (which is a component) of the `qemu_edu`
    driver.
*   Start the Fuchsia debugger ([`zxdb`][zxdb-user-guide])and connect it to
    the emulator instance.
*   Attach the debugger to the driver host.
*   Set a breakpoint on the driver’s code.
*   Run the tools component, which triggers the driver to execute its
    instructions.
*   Step through the driver’s code.

Do the following:

1. View the list of the running driver hosts:

   ```posix-terminal
   tools/ffx driver list-hosts
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx driver list-hosts
   Driver Host: 5825
       fuchsia-boot:///#meta/bus-pci.cm
       fuchsia-boot:///#meta/goldfish-display.cm
       fuchsia-boot:///#meta/goldfish.cm
       fuchsia-boot:///#meta/goldfish_control.cm
       fuchsia-boot:///#meta/goldfish_sensor.cm
       fuchsia-boot:///#meta/goldfish_sync.cm
       fuchsia-boot:///#meta/intel-rtc.cm
       fuchsia-boot:///#meta/pc-ps2.cm
       fuchsia-boot:///#meta/platform-bus-x86.cm
       fuchsia-boot:///display-coordinator#meta/display-coordinator.cm
       fuchsia-boot:///hid#meta/hid.cm
       fuchsia-boot:///platform-bus#meta/platform-bus.cm
   ...

   Driver Host: 21574
       fuchsia-pkg://fuchsia.com/virtual_audio#meta/virtual_audio_driver.cm

   Driver Host: 58240
       fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm
   ```

   Make a note of the PID of the `qemu_edu` driver host (`58240` in the
   example above).

1. Start the Fuchsia debugger:

   ```posix-terminal
   tools/ffx debug connect
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx debug connect
   Connecting (use "disconnect" to cancel)...
   Connected successfully.
   👉 To get started, try "status" or "help".
   [zxdb]
   ```

1. Attach the debugger to the `qemu_edu` driver host:

   <pre class="devsite-click-to-copy">
   <span class="no-select">[zxdb] </span>attach <var>PID</var>
   </pre>

   Replace `PID` with the PID of the `qemu_edu` driver host identified
   in step 1, for example:

   ```none {:.devsite-disable-click-to-copy}
   [zxdb] attach 58240
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   [zxdb] attach 58240
   Attached Process 1 state=Running koid=58240 name=driver_host2.cm component=driver_host2.cm
   Loading 13 modules for driver_host2.cm Downloading symbols...
   Done.
   Symbol downloading complete. 8 succeeded, 0 failed.
   [zxdb]
   ```

1. Set a breakpoint at the driver’s `HandleIrq()` function:

   <pre class="devsite-click-to-copy">
   <span class="no-select">[zxdb] </span>break QemuEduDevice::HandleIrq
   </pre>

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   [zxdb] break QemuEduDevice::HandleIrq
   Created Breakpoint 1 @ QemuEduDevice::HandleIrq
      95 // Respond to INTx interrupts triggered by the device, and return the compute result.
      96 void QemuEduDevice::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq,
    {{ '<strong>' }}◉ 97                               zx_status_t status, const zx_packet_interrupt_t* interrupt) { {{ '</strong>' }}
      98   irq_.ack();
      99   if (!pending_callback_.has_value()) {
   [zxdb]
   ```

1. In different terminal, run the tools component (using `fact 12` as input):

   Note:  In this new terminal, make sure that you change to the same work
   directory (for instance, `cd $HOME/fuchsia-drivers`).

   ```posix-terminal
   tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ```

   Unlike in the previous section, this command now waits after printing
   output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ...
   Publishing packages: [PosixPath('src/qemu_edu/tools/eductl.far')]
   Published 1 packages
   Running task: pkg.eductl_tool.run_only (step 3/4)
   ```

   In the `zxdb` terminal, verify that the debugger is stopped at the
   `HandleIrq()` function, for example:

   ```none {:.devsite-disable-click-to-copy}
      95 // Respond to INTx interrupts triggered by the device, and return the compute result.
      96 void QemuEduDevice::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq,
    {{ '<strong>' }}▶ 97                               zx_status_t status, const zx_packet_interrupt_t* interrupt) { {{ '</strong>' }}
      98   irq_.ack();
      99   if (!pending_callback_.has_value()) {
   🛑 thread 3 on bp 1 edu_device::QemuEduDevice::HandleIrq(edu_device::QemuEduDevice*, async_dispatcher_t*, async::IrqBase*, zx_status_t, zx_packet_interrupt_t const*) • edu_device.cc:97
   [zxdb]
   ```

1. In the `zxdb` terminal, view the source code around the current breakpoint:

   <pre class="devsite-click-to-copy">
   <span class="no-select">[zxdb] </span>list
   </pre>

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   [zxdb] list
       92   pending_callback_ = std::move(callback);
       93 }
       94
       95 // Respond to INTx interrupts triggered by the device, and return the compute result.
       96 void QemuEduDevice::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq,
    {{ '<strong>' }}▶  97                               zx_status_t status, const zx_packet_interrupt_t* interrupt) { {{ '</strong>' }}
       98   irq_.ack();
       99   if (!pending_callback_.has_value()) {
      100     FDF_LOG(ERROR, "Received unexpected interrupt!");
      101     return;
      102   }
      103   auto callback = std::move(*pending_callback_);
      104   pending_callback_ = std::nullopt;
      105   if (status != ZX_OK) {
      106     FDF_SLOG(ERROR, "Failed to wait for interrupt", KV("status", zx_status_get_string(status)));
      107     callback(zx::error(status));
   [zxdb]
   ```

1. In the `zxdb` terminal, step through the `HandleIrq()` function
   using the `next` command until the value of `factorial` is computed and
   the callback is invoked (that is, until the line 128 is reached):

   <pre class="devsite-click-to-copy">
   <span class="no-select">[zxdb] </span>next
   </pre>

   The last `next` command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   [zxdb] next
      126   FDF_SLOG(INFO, "Replying with", KV("factorial", factorial));
      127   callback(zx::ok(factorial));
    {{ '<strong>' }}▶ 128 } {{ '</strong>' }}
      129 // [END compute_factorial]
      130
   🛑 thread 3 edu_device::QemuEduDevice::HandleIrq(edu_device::QemuEduDevice*, async_dispatcher_t*, async::IrqBase*, zx_status_t, zx_packet_interrupt_t const*) • edu_device.cc:128
   [zxdb]
   ```

   In the other terminal, after the `HandleIrq()` function invokes the
   callback, verify that `eductl_tool` prints the factorial result and exits:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/tools:pkg.eductl_tool -- fact 12
   ...
   Publishing packages: [PosixPath('src/qemu_edu/tools/eductl.far')]
   Published 1 packages
   Running task: pkg.eductl_tool.run_only (step 3/4)
   {{ '<strong>' }}Factorial(12) = 479001600{{ '</strong>' }}
   ...
   ```

1. In the `zxdb` terminal, type `exit` or press `Ctrl-D` to exit the debugger.

   Note: For more information on usages and best practices on `zxdb`, see the
   [zxdb user guide][zxdb-user-guide].

<!-- Reference links -->

[zxdb-user-guide]: /docs/development/debugger/README.md
