# Drivers

Important: This section contains information that is specific to the new
version of the driver framework (DFv2).

Drivers provide software interfaces for communicating with hardware (or virtual)
devices that are embedded in or connected to a system. In Fuchsia, drivers are
user-space [components][components]. Like any other Fuchsia component, a driver
is software that exposes and receives FIDL capabilities to and from other
components in the system. Using these FIDL calls, Fuchsia components interact
with drivers, which are bound to specific devices in the system.

Similar to Fuchsia’s component framework, which manages Fuchsia components, the
[driver framework][driver-framework] manages the lifecycle and topology of
all devices (known as [nodes][nodes]) and drivers in a Fuchsia system.

*  [Driver framework (DFv2)][driver-framework]
*  [Comparison between DFv1 and DFv2][dfv1-and-dfv2]
*  [Drivers and nodes][nodes]
*  [Driver binding][driver-binding]
*  [Driver communication][driver-communication]
*  [Mapping a device's memory in a driver][mapping-memory]
*  [Driver dispatcher and threads][driver-dispatcher]
*. [Driver dispatcher performance][driver-dispatcher-performance]

<!-- Reference links -->

[components]: /docs/concepts/components/v2/README.md
[driver-framework]: driver_framework.md
[dfv1-and-dfv2]: comparison_between_dfv1_and_dfv2.md
[nodes]: drivers_and_nodes.md
[driver-binding]: driver_binding.md
[driver-communication]: driver_communication.md
[mapping-memory]: mapping-a-devices-memory-in-a-driver.md
[driver-dispatcher]: driver-dispatcher-and-threads.md
[driver-dispatcher-performance]: driver-dispatcher-performance.md

