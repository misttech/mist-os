The `core` image (launched in the [Start the emulator](#start-the-emulator)
section above) is configured to create a virtual device named
[`edu`][edu-device]{:.external}, which is an educational device for writing
drivers. In the previous section, when the emulator started, Fuchsia’s driver
framework detected this `edu` device in the system, but it wasn’t able to find
a driver that could serve the `edu` device. So the `edu` device was left unmatched.

In this section, we'll build and publish the [`qemu_edu`][qemu-edu]{:.external}
sample driver (which is a Fuchsia component). Upon detecting a new driver, the
driver framework will discover that this new `qemu_edu` driver is a match for
the `edu` device. Once matched, the `qemu_edu` driver starts providing the `edu`
device’s services (capabilities) to other components in the system – one of the
services provided by the `edu` device is that it computes a factorial given
an integer.

The tasks include:

*   View the drivers that are currently loaded in the emulator instance.
*   Build and publish the `qemu_edu` driver component.
*   Verify that the `qemu_edu` driver is loaded to the emulator instance.
*   View detailed information on the `qemu_edu` component.

Do the following:

1. View the list of the currently loaded drivers:

   ```posix-terminal
   tools/ffx driver list --loaded
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx driver list --loaded
   fuchsia-boot:///hid#meta/hid.cm
   fuchsia-boot:///hid-input-report#meta/hid-input-report.cm
   fuchsia-boot:///network-device#meta/network-device.cm
   fuchsia-boot:///platform-bus#meta/platform-bus.cm
   fuchsia-boot:///ramdisk#meta/ramdisk.cm
   fuchsia-boot:///sysmem#meta/sysmem.cm
   fuchsia-boot:///#meta/pc-ps2.cm
   fuchsia-boot:///#meta/goldfish-display.cm
   fuchsia-boot:///#meta/bus-pci.cm
   fuchsia-boot:///#meta/platform-bus-x86.cm
   fuchsia-boot:///#meta/goldfish.cm
   fuchsia-boot:///#meta/qemu-audio-codec.cm
   fuchsia-boot:///#meta/goldfish_sync.cm
   fuchsia-boot:///#meta/virtio_block.cm
   fuchsia-boot:///#meta/intel-hda.cm
   fuchsia-boot:///#meta/goldfish_address_space.cm
   fuchsia-boot:///#meta/goldfish_control.cm
   fuchsia-boot:///#meta/virtio_netdevice.cm
   fuchsia-boot:///#meta/intel-rtc.cm
   fuchsia-boot:///#meta/ahci.cm
   fuchsia-boot:///#meta/virtio_input.cm
   fuchsia-boot:///#meta/goldfish_sensor.cm
   fuchsia-pkg://fuchsia.com/fake-battery#meta/fake_battery.cm
   fuchsia-pkg://fuchsia.com/virtual_audio#meta/virtual_audio_driver.cm
   fuchsia-boot:///block-core#meta/block.core.cm
   fuchsia-boot:///display-coordinator#meta/display-coordinator.cm
   fuchsia-boot:///fvm#meta/fvm.cm
   ```

2. Build and publish the `qemu_edu` driver component:

   ```posix-terminal
   tools/bazel run //src/qemu_edu/drivers:pkg.component
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/bazel run //src/qemu_edu/drivers:pkg.component
   ...
   Registering bazel.pkg.publish.anonymous to target device fuchsia-emulator
   Publishing packages: [PosixPath('src/qemu_edu/drivers/qemu_edu.far')]
   Published 1 packages
   Running task: pkg.component.run_only (step 3/4)
   Registering fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm, restarting driver hosts, and attempting to bind to unbound nodes
   Successfully bound:
   Node 'dev.sys.platform.pt.PCI0.bus.00_06.0.00_06.0':
   Driver 'Some(
       "fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm",
   )'
   ...
   ```

3. Verify that the `qemu_edu` driver is now loaded to the Fuchsia emulator
   instance:

   ```posix-terminal
   tools/ffx driver list --loaded
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx driver list --loaded
   fuchsia-boot:///hid-input-report#meta/hid-input-report.cm
   fuchsia-boot:///network-device#meta/network-device.cm
   fuchsia-boot:///platform-bus#meta/platform-bus.cm
   fuchsia-boot:///ramdisk#meta/ramdisk.cm
   fuchsia-boot:///sysmem#meta/sysmem.cm
   fuchsia-boot:///#meta/pc-ps2.cm
   fuchsia-boot:///#meta/goldfish-display.cm
   fuchsia-boot:///#meta/bus-pci.cm
   fuchsia-boot:///#meta/platform-bus-x86.cm
   fuchsia-boot:///#meta/goldfish.cm
   fuchsia-boot:///#meta/qemu-audio-codec.cm
   fuchsia-boot:///#meta/goldfish_sync.cm
   fuchsia-boot:///#meta/virtio_block.cm
   fuchsia-boot:///#meta/intel-hda.cm
   fuchsia-boot:///#meta/goldfish_address_space.cm
   fuchsia-boot:///#meta/goldfish_control.cm
   fuchsia-boot:///#meta/virtio_netdevice.cm
   fuchsia-boot:///#meta/intel-rtc.cm
   fuchsia-boot:///#meta/ahci.cm
   fuchsia-boot:///#meta/virtio_input.cm
   fuchsia-boot:///#meta/goldfish_sensor.cm
   fuchsia-pkg://fuchsia.com/fake-battery#meta/fake_battery.cm
   fuchsia-pkg://fuchsia.com/virtual_audio#meta/virtual_audio_driver.cm
   {{ '<strong>' }}fuchsia-pkg://bazel.pkg.component/qemu_edu#meta/qemu_edu.cm{{ '</strong>' }}
   fuchsia-boot:///block-core#meta/block.core.cm
   fuchsia-boot:///display-coordinator#meta/display-coordinator.cm
   fuchsia-boot:///fvm#meta/fvm.cm
   fuchsia-boot:///hid#meta/hid.cm
   ```

   Notice that the `qemu_edu` driver is shown in the loaded drivers list.

4. View the `qemu_edu` component information:

   ```posix-terminal
   tools/ffx component show qemu_edu.cm
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx component show qemu_edu.cm
                    Moniker:  bootstrap/full-pkg-drivers:dev.sys.platform.pt.PCI0.bus.00_06.0.00_06.0
                        URL:  fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm
                Environment:  full-pkg-driver-env
                Instance ID:  None
            Component State:  Resolved
               Resolved URL:  fuchsia-pkg://bazel.pkg.publish.anonymous/qemu_edu#meta/qemu_edu.cm
     Namespace Capabilities:  /svc/fuchsia.logger.LogSink
                              /svc/fuchsia.hardware.pci.Service
       Exposed Capabilities:  examples.qemuedu.Service
                Merkle root:  2a1d06a05f31b98837a7760f0d227942813ff375dce873a846ae8153600e7bb6
            Execution State:  Running
               Start reason:  Instance is in a single_run collection
      Outgoing Capabilities:  examples.qemuedu.Service
                    Runtime:  ELF
              Running since:  169224187887 ticks
                     Job ID:  58210
                 Process ID:  58240
   ```

5. View the device logs of the `qemu-edu` driver:

   ```posix-terminal
   tools/ffx log --tags qemu-edu dump
   ```

   This command prints output similar to the following:

   ```none {:.devsite-disable-click-to-copy}
   $ tools/ffx log --tags qemu-edu dump
   ...
   [00331.380217][full-pkg-drivers:dev.sys.platform.pt.PCI0.bus.00_06.0.00_06.0][driver,qemu-edu] INFO: [src/qemu_edu/drivers/qemu_edu.cc(45)] edu device version minor=0 major=1
   ```

<!-- Reference links -->

[edu-device]: https://fuchsia.googlesource.com/third_party/qemu/+/refs/heads/main/docs/specs/edu.txt
[qemu-edu]: https://fuchsia.googlesource.com/sdk-samples/drivers/+/refs/heads/main/src/qemu_edu/
