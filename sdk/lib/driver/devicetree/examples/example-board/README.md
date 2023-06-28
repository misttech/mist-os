# Devicetree example board driver

This is an example devicetree based board driver implementation which can be used as a reference.

## Board driver

The board driver initializes the devicetree manager library with the incoming namespace in order to
provide connection to |fuchsia_boot::Items| protocol. Once the manager is reads the devicetree using
the |fuchsia_boot::Items| protocol, it is used to walk the devicetree and collect node properties.
During the devicetree walk, several visitors can be provided to parse and collect node properties. In
this example we are using the default visitor set which parses all the standard bindings supported in
Fuchsia. See `visitors` folder for more details. Once the walk is completed, the manager is invoked to
publish all the devices/nodes.

## Passing the DTB

Typically the devicetree blob (DTB) is passed down by the bootloader to the kernel as a |
ZBI_TYPE_DEVICETREE|
item and made available to the board driver via |fuchsia_boot::Items| protocol. In boards where the
bootloader is not yet capable of passing the DTB (typically during board bringup), the kernel ZBI can
be appended with the devicetree blob at build time using |zbi_input| and |kernel_zbi_extra_deps|.
See `BUILD.gn` for more details.

## Testing

The integration test creates a test realm with driver framework, platform bus and the board
driver components running in it. The DTB is passed as a test data resource and is provided to the
board driver through the fake |fuchsia_boot::Items| protocol implemented in the `board-test-helper`
library. The test creates a platform bus device with specified VID, PID to which the board driver will
bind to. The board driver would then parse the devicetree and create other child nodes. A typical test
would be to enumerate the nodes created.
