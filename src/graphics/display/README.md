# The Fuchsia display stack

The code here is responsible for driving the display engine hardware in the
systems supported by Fuchsia.

The [Display hardware overview][display-hardware-overview] document introduces
many terms needed to understand display code, and is a good background reading.

## Stack overview

### The FIDL interface for display clients

The official entry point to the display driver stack is the
[`fuchsia.hardware.display/Coordinator`][display-coordinator-fidl] FIDL
interface. This FIDL interface will eventually become a part of the
[Fuchsia System Interface][fuchsia-system-interface], and will be covered by ABI
stability guarantees.

The interface's consumers are called **display clients**. This is a reference to
the fact that consumers use FIDL clients for the Coordinator interface.

This interface is currently only exposed inside the Fuchsia platform source
tree. The existing consumers of the interface are [Scenic][scenic],
[virtcon][virtcon], and the [recovery UI][recovery-ui]. The last two consumers
use the [Carnelian][carnelian] library. There are no plans of supporting any new
consumers.

### The Display Coordinator

The Display Coordinator maintains the state needed to multiplex the display
engine hardware across multiple display clients. The Coordinator's design aims
to allow for multiple display clients and multiple display engine drivers, but
this goal was sacrificed in a few places, in the name of expediency.

Whenever possible, data processing is implemented in the Coordinator, reducing
the implementation complexity of hardware-specific display drivers. For example,
the Coordinator parses [EDID (Extended Display Identification Data)][edid], and
the display drivers are only responsible for reading the EDID bytes from the
display hardware.

The Coordinator implements the `fuchsia.hardware.display/Coordinator` FIDL
interface, described above, to serve requests from display clients such as
Scenic.

The Display Coordinator is currently implemented as the `coordinator` driver in
the Fuchsia platform source tree. The `fuchsia.hardware.display/Coordinator`
FIDL interface is currently served under the `display-coordinator` class in
[`devfs`][devfs].

### The FIDL and Banjo interfaces for display engine drivers

The Coordinator will soon use the
[`fuchsia.hardware.display.engine/Engine`][display-engine-fidl] FIDL interface
to communicate with hardware-specific display drivers. This FIDL interface will
soon become a part of the Fuchsia System Interface, and will be covered by API
stability guarantees.

The Coordinator currently also uses the
[`fuchsia.hardware.display.controller/DisplayControllerImpl`][display-controller-banjo]
Banjo interface to communicate with display engine drivers. This interface is
associated with the `DISPLAY_CONTROLLER_IMPL` [protocol][dfv1-protocol]
identifier. This Banjo interface will be removed when
[DFv1 (Driver Framework v1)][dfv1] support is removed from the display stack.

### Display engine drivers

Display engine drivers contain hardware-specific logic for driving a display
engine.

All new display drivers shall be built on top of
[DFv2 (Driver Framework v2)][dfv2]. Each DFv2 driver exposes an implementation
of the `fuchsia.hardware.display.engine/Engine` FIDL interface to integrate with
the rest of the display stack. Each driver uses the
[DFv2 (Driver Framework v2) FIDL interface][dfv2-fidl] to communicate with the
Driver Framework.

At the time of this writing, existing display drivers are built on top of DFv1.
These drivers will be [migrated to DFv2][dfv2-migration] or removed from the
Fuchsia platform source tree.

[carnelian]: /src/bringup/bin/virtcon/README.md
[devfs]: /docs/concepts/drivers/driver_communication.md#service_discovery_using_devfs
[dfv1]: /docs/development/drivers/concepts/fdf.md
[dfv1-protocol]: /docs/development/drivers/concepts/device_driver_model/protocol.md
[dfv2]: /docs/concepts/drivers/driver_framework.md
[dfv2-fidl]: /docs/concepts/drivers/driver_framework.md#fidl_interface
[dfv2-migration]: /docs/development/drivers/migration/migrate-from-dfv1-to-dfv2.md
[display-hardware-overview]: docs/hardware.md
[display-controller-banjo]: /sdk/banjo/fuchsia.hardware.display.controller/display-controller.fidl
[display-coordinator-fidl]: /sdk/fidl/fuchsia.hardware.display/coordinator.fidl
[display-engine-fidl]: /sdk/fidl/fuchsia.hardware.display.engine/engine.fidl
[edid]: https://en.wikipedia.org/wiki/Extended_Display_Identification_Data
[fuchsia-system-interface]: /docs/concepts/packages/system.md
[recovery-ui]: /src/recovery/lib/recovery-ui/BUILD.gn
[scenic]: /docs/concepts/ui/scenic/index.md
[virtcon]: /src/bringup/bin/virtcon/README.md
