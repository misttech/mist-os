# View driver information

The `ffx driver` command can retrieve various types of information about drivers
on a Fuchsia device.

## Concepts {:#concepts}

The `ffx driver` command can retrieve information related to drivers that are
currently [available](#view-drivers) or [running](#view-running-drivers) on
your target [Fuchsia device][flash-a-device] (or [emulator][fuchsia-emulator]).
However, the `ffx driver` command expects that you can establish an
[SSH connection][ssh-connection] to the target Fuchsia device from your host
machine. To verify this connection to the device, you can run the
[`ffx target show`][ffx-target-show] command.

Before using the `ffx driver` command, it is recommended that you familiarize
yourself with the fundamental concepts in [Driver framework (DFv2)][dfv2],
particularly the following:

- [Device nodes][device-nodes] - A device node represents a hardware component,
  a virtual device, or a part of a hardware device.
- [Node topology][node-topology] - A node topology describes the parent-child
  relationships between device nodes in the system.
- [Driver host][driver-host] - A driver host, which runs as a Fuchsia component,
  provides isolation between drivers in a Fuchsia system. In Fuchsia, every
  driver lives in a driver host, and more than one driver can be co-located
  within a single driver host.

## View drivers {:#view-drivers}

To view all drivers available on your Fuchsia device, run the following command:

```posix-terminal
ffx driver list
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list
fuchsia-boot:///alc5663#meta/alc5663.cm
fuchsia-boot:///asix-88179#meta/asix-88179.cm
fuchsia-boot:///asix-88772b#meta/asix-88772b.cm
fuchsia-boot:///block-core#meta/block.core.cm
fuchsia-boot:///bt-transport-usb#meta/bt-transport-usb.cm
fuchsia-boot:///bus-pci#meta/bus-pci.cm
fuchsia-boot:///buttons#meta/buttons.cm
fuchsia-boot:///clock#meta/clock.cm
fuchsia-boot:///ctaphid#meta/ctaphid.cm
fuchsia-boot:///display-coordinator#meta/display-coordinator.cm
fuchsia-boot:///e1000#meta/e1000.cm
fuchsia-boot:///ftdi#meta/ftdi.cm
fuchsia-boot:///fvm#meta/fvm.cm
...
```

To view all drivers available on your Fuchsia device with more detailed
information, run the command with the -`v` flag:

```posix-terminal
ffx driver list -v
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list -v
URL       : fuchsia-pkg://fuchsia.com/iwlwifi#meta/iwlwifi.cm
DF Version: 2
Device Categories: [misc]
Bind rules bytecode:
  fuchsia.BIND_FIDL_PROTOCOL == 4
  fuchsia.BIND_PCI_VID == 32902
  Jump if fuchsia.BIND_PCI_DID == 2394 to ??
  Jump if fuchsia.BIND_PCI_DID == 2395 to ??
  Jump if fuchsia.BIND_PCI_DID == 9469 to ??
  Jump if fuchsia.BIND_PCI_DID == 9510 to ??
  Jump if fuchsia.BIND_PCI_DID == 41200 to ??
  Abort
  Label ??
  fuchsia.BIND_COMPOSITE == 1

URL       : fuchsia-boot:///virtio_rng#meta/virtio_rng.cm
DF Version: 1
Device Categories: [misc]
Bind rules bytecode:
Node (primary): pci
  fuchsia.BIND_FIDL_PROTOCOL == 4
  fuchsia.BIND_PCI_VID == 6900
  Jump if fuchsia.BIND_PCI_DID == 4164 to ??
  Jump if fuchsia.BIND_PCI_DID == 4101 to ??
...
```

### View running drivers {:#view-running-drivers}

To view all drivers that are currently running (loaded) on your Fuchsia
device, run the command with the -`loaded` flag:

```posix-terminal
ffx driver list --loaded
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list --loaded
fuchsia-boot:///#meta/block.core.cm
fuchsia-boot:///#meta/bus-pci.cm
fuchsia-boot:///#meta/display.cm
fuchsia-boot:///#meta/fvm.cm
fuchsia-boot:///#meta/goldfish-display.cm
fuchsia-boot:///#meta/goldfish.cm
fuchsia-boot:///#meta/goldfish_address_space.cm
fuchsia-boot:///#meta/goldfish_control.cm
fuchsia-boot:///#meta/goldfish_sensor.cm
fuchsia-boot:///#meta/goldfish_sync.cm
fuchsia-boot:///#meta/hid-input-report.cm
fuchsia-boot:///#meta/hid.cm
fuchsia-boot:///#meta/intel-hda.cm
...
```

## View driver hosts {:#view-driver-hosts}

To view all [driver hosts][driver-host] running on your Fuchsia device
as well as the drivers they host, run the following command:

```posix-terminal
ffx driver list-hosts
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-hosts
Driver Host: 5416
    fuchsia-boot:///#meta/bus-pci.cm
    fuchsia-boot:///#meta/display.cm
    fuchsia-boot:///#meta/goldfish-display.cm
    fuchsia-boot:///#meta/goldfish.cm
    fuchsia-boot:///#meta/goldfish_control.cm
...

Driver Host: 8248
    fuchsia-boot:///#meta/intel-rtc.cm

Driver Host: 8317
    fuchsia-boot:///#meta/pc-ps2.cm

Driver Host: 9604
    fuchsia-boot:///#meta/block.core.cm
    fuchsia-boot:///#meta/fvm.cm
    fuchsia-boot:///#meta/virtio_block.cm
...
```

## View the node topology {:#view-the-node-topology}

To view the entire [node topology][node-topology] of your Fuchsia device,
run the following command:

```posix-terminal
ffx driver dump
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver dump
[dev] pid=5521 fuchsia-boot:///platform-bus#meta/platform-bus.cm
  [sys] pid=None unbound
    [platform] pid=None unbound
      [ram-disk] pid=7584 fuchsia-boot:///ramdisk#meta/ramdisk.cm
        [ramctl] pid=None unbound
      [00_00_2e] pid=None unbound
      [virtual-audio] pid=25117 fuchsia-pkg://fuchsia.com/virtual_audio#meta/virtual_audio_driver.cm
        [virtual_audio] pid=None unbound
      [bt-hci-emulator] pid=None unbound
      [fake-battery] pid=24707 fuchsia-pkg://fuchsia.com/fake-battery#meta/fake_battery.cm
        [fake-battery] pid=None unbound
        [power-simulator] pid=None unbound
      [pt] pid=5521 fuchsia-boot:///platform-bus-x86#meta/pl
...
```

### View the node topology under a specific node {:#view-the-node-topology-under-a-specific-node}

To view only a subgraph of the node topology under a specific node,
run the following command:

```posix-terminal
ffx driver dump <NODE_NAME>
```

Replace `NODE_NAME` with the name of your target node, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx driver dump goldfish-control
[goldfish-control] pid=5521 fuchsia-boot:///goldfish_display#meta/goldfish-display.cm
  [goldfish-display] pid=5521 fuchsia-boot:///display-coordinator#meta/display-coordinator.cm
    [display-coordinator] pid=None unbound
...
```

### Graph the node topology {:#graph-the-node-topology}

To graph the node topology, run the following command:

```posix-terminal
ffx driver driver dump --graph
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver driver dump --graph
digraph {
     forcelabels = true; splines="ortho"; ranksep = 1.2; nodesep = 0.5;
     node [ shape = "box" color = " #2a5b4f" penwidth = 2.25 fontname = "prompt medium" fontsize = 10 margin = 0.22 ];
     edge [ color = " #37474f" penwidth = 1 style = dashed fontname = "roboto mono" fontsize = 10 ];
     "3675787314320" [label="dev"]
     "3675787305008" [label="sys"]
     "3675787315872" [label="platform"]
     "3675787309664" [label="ram-disk"]
     "3675787306560" [label="00_00_2e"]
     "3675787308112" [label="virtual-audio"]
     "3675787312768" [label="bt-hci-emulator"]
     "3675787311216" [label="fake-battery"]
     "3675787325184" [label="pt"]
     "3675787317424" [label="00_00_1b"]
...
```

To convert this output to a `png` file, you can install the
[`dot`][dot]{:.external} command and pass the output to generate
the `png` file. Alternatively, you can paste the output to
[GraphViz][graphviz]{:.external}.

## View device nodes {:#view-device-nodes}

To view the properties of all [device nodes][device-nodes] on your
Fuchsia device, run the following command:

```posix-terminal
ffx driver list-devices
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-devices
dev
dev.sys
dev.sys.platform
dev.sys.platform.ram-disk
dev.sys.platform.00_00_2e
dev.sys.platform.virtual-audio
dev.sys.platform.bt-hci-emulator
dev.sys.platform.fake-battery
dev.sys.platform.pt
dev.sys.platform.00_00_1b
dev.sys.platform.ram-disk.ramctl
dev.sys.platform.virtual-audio.virtual_audio
dev.sys.platform.fake-battery.fake-battery
dev.sys.platform.fake-battery.power-simulator
dev.sys.platform.pt.PCI0
dev.sys.platform.pt.acpi
dev.sys.platform.00_00_1b.sysmem
dev.sys.platform.pt.PCI0.bus
dev.sys.platform.pt.acpi.acpi-_SB_
dev.sys.platform.pt.acpi.acpi-_TZ_
dev.sys.platform.00_00_1b.sysmem.sysmem-banjo
dev.sys.platform.00_00_1b.sysmem.sysmem-fidl
dev.sys.platform.pt.PCI0.bus.00_00.0
dev.sys.platform.pt.PCI0.bus.00_01.0
dev.sys.platform.pt.PCI0.bus.00_02.0
...
```

To view the properties of device nodes with more detailed information,
run the command with the -`v` flag:

```posix-terminal
ffx driver list-devices -v
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-devices -v
...
Name     : i2c-child
Moniker  : root.sys.platform.platform-passthrough.acpi.acpi-FWCF.i2c-child
Driver   : None
3 Properties
[ 1/  3] : Key "fuchsia.hardware.i2c"          Value Enum(fuchsia.hardware.i2c.Device.ZirconTransport)
[ 2/  3] : Key fuchsia.BIND_I2C_ADDRESS        Value 0x0000ff
[ 3/  3] : Key "fuchsia.driver.framework.dfv2" Value true
...
```

### View device nodes under a specific path {:#view-device-nodes-under-a-specific-path}

To view device nodes filtered by a specific path in the node topology,
include the exact topological path or device name to the command:

```posix-terminal
ffx driver list-devices <NODE>
```

Replace `NODE` with a topological path or device name. For example,
the example below shows that the results are filtered by the
topological path at `acpi`:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-devices acpi
dev.sys.platform.pt.acpi
dev.sys.platform.pt.acpi.acpi-_SB_
dev.sys.platform.pt.acpi.acpi-_TZ_
dev.sys.platform.pt.acpi.acpi-_SB_.pt
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-PCI0
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-HPET
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-LNKE
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-LNKF
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-LNKG
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-LNKH
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-GSIE
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-GSIF
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-GSIG
dev.sys.platform.pt.acpi.acpi-_SB_.acpi-GSIH
...
```

You can also combine this argument with the -`v` flag for
more detailed information, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-devices acpi -v
Name     : acpi
Moniker  : dev.sys.platform.pt.acpi
Driver   : unbound
2 Properties
[ 1/  2] : Key fuchsia.BIND_PROTOCOL          Value 0x00001c
[ 2/  2] : Key "fuchsia.platform.DRIVER_FRAMEWORK_VERSION" Value 0x000002
1 Offers
Service: fuchsia.driver.compat.Service
  Source: dev.sys.platform.pt
  Instances: default

Name     : acpi-_SB_
Moniker  : dev.sys.platform.pt.acpi.acpi-_SB_
Driver   : unbound
2 Properties
[ 1/  2] : Key fuchsia.BIND_PROTOCOL          Value 0x00001c
[ 2/  2] : Key "fuchsia.platform.DRIVER_FRAMEWORK_VERSION" Value 0x000002
1 Offers
Service: fuchsia.driver.compat.Service
  Source: dev.sys.platform.pt
  Instances: default
...
```

## View composite nodes {:#view-composite-nodes}

To view all composite nodes on your Fuchsia device,
run the following command:

```posix-terminal
ffx driver list-composites
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-composites
...
acpi-GFRO-composite
acpi-CPUS-composite
acpi-_TZ_-composite
goldfish-control-2
00:00.0
00:01.0
00:02.0
00:03.0
00:04.0
00:05.0
00:06.0
00:0b.0
...
```

To view composite nodes with more detailed information,
run the command with the -`v` flag:

```posix-terminal
ffx driver list-composites -v
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-composites -v
...
Name     : 00_02_0
Driver   : fuchsia-boot:///#meta/virtio_block.cm
Device   : dev/sys/platform/pt/PCI0/bus/00:02.0/00_02_0
Parents  : 3
Parent 0 : sysmem
   Device : dev/sys/platform/00:00:1b/sysmem/sysmem-fidl
Parent 1 : pci (Primary)
   Device : dev/sys/platform/pt/PCI0/bus/00:02.0
Parent 2 : acpi
   Device : dev/sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-S10_/pt
...
```

## View composite node specifications {:#view-composite-node-specifications}

To view all composite node specifications on your Fuchsia
device, run the following command:

```posix-terminal
ffx driver list-composite-node-specs
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-composite-node-specs
...
00_1f_0             : None
00_1f_2             : fuchsia-boot:///ahci#meta/ahci.cm
00_06_0             : None
00_02_0             : fuchsia-boot:///virtio_block#meta/virtio_block.cm
00_1f_3             : None
00_05_0             : fuchsia-boot:///virtio_input#meta/virtio_input.cm
00_0b_0             : fuchsia-boot:///goldfish_address_space#meta/goldfish_address_space.cm
00_01_0             : fuchsia-boot:///intel-hda#meta/intel-hda.cm
00_03_0             : fuchsia-boot:///virtio_input#meta/virtio_input.cm
00_04_0             : fuchsia-boot:///virtio_netdevice#meta/virtio_netdevice.cm
00_00_0             : None
...
```

To view composite node specifications with more detailed
information, run the command with the -`v` flag:

```posix-terminal
ffx driver list-composite-node-specs -v
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx driver list-composite-node-specs -v
...
Name      : ft3x27_touch
Driver    : fuchsia-boot:///#meta/focaltech.cm
Nodes     : 2
Node 0    : "i2c" (Primary)
  3 Bind Rules
  [ 1/ 3] : Accept "fuchsia.BIND_FIDL_PROTOCOL" { 0x000003 }
  [ 2/ 3] : Accept "fuchsia.BIND_I2C_BUS_ID" { 0x000001 }
  [ 3/ 3] : Accept "fuchsia.BIND_I2C_ADDRESS" { 0x000038 }
  2 Properties
  [ 1/ 2] : Key "fuchsia.BIND_FIDL_PROTOCOL"   Value 0x000003
  [ 2/ 2] : Key "fuchsia.BIND_I2C_ADDRESS"     Value 0x000038
Node 1    : "gpio-int"
  2 Bind Rules
  [ 1/ 2] : Accept "fuchsia.BIND_PROTOCOL" { 0x000014 }
  [ 2/ 2] : Accept "fuchsia.BIND_GPIO_PIN" { 0x000004 }
  2 Properties
  [ 1/ 2] : Key "fuchsia.BIND_PROTOCOL"        Value 0x000014
  [ 2/ 2] : Key "fuchsia.gpio.FUNCTION"        Value "fuchsia.gpio.FUNCTION.TOUCH_INTERRUPT"
...
```

## Appendices

### Register a component as a driver {:#register-a-component-as-a-driver}

To register a component as a driver to your Fuchsia device,
run the following command:

```posix-terminal
ffx driver register <URL>
```

Replace `URL` with a component URL from your Fuchsia package
server, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx driver register fuchsia-pkg://fuchsia.com/my_example#meta/my_new_driver.cm
```

### Disable a driver {:#disable-a-driver}

To disable (that is, de-register) a driver from your Fuchsia device,
run the following command:

```posix-terminal
ffx driver disable <URL>
```

Replace `URL` with a component URL from your Fuchsia package
server, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx driver disable fuchsia-pkg://fuchsia.com/my_example#meta/my_driver.cm
```

<!-- Reference links -->

[flash-a-device]: /docs/development/sdk/ffx/flash-a-device.md
[fuchsia-emulator]: /docs/development/sdk/ffx/start-the-fuchsia-emulator.md
[ssh-connection]: /docs/development/sdk/ffx/create-ssh-keys-for-devices.md
[ffx-target-show]: /docs/development/sdk/ffx/view-device-information.md#get_detailed_information_from_a_device
[dfv2]: /docs/concepts/drivers/driver_framework.md
[device-nodes]: /docs/concepts/drivers/drivers_and_nodes.md
[node-topology]: /docs/concepts/drivers/drivers_and_nodes.md#node_topology
[driver-host]: /docs/concepts/drivers/driver_framework.md#driver_host
[dot]: https://www.mankier.com/1/dot
[graphviz]: https://dreampuf.github.io/GraphvizOnline/#digraph
