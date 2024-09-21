# SPMI overview

## Concepts

System Power Management Interface, or SPMI, is a bus on which up to four
controller devices can communicate with up to sixteen target devices to
accomplish different power management tasks. Each controller and target device
has an address space of up to 16 bits made up of 8-bit registers, the functions
of which are defined by the hardware vendor.

While not part of the SPMI specification, some target devices that have multiple
functions can be further divided into sub-target devices, which have a limited
view into the target's address space. Having a logical separation of device
functions allows the software drivers for them to be separate as well.

## Writing an SPMI client driver

Using SPMI from a client driver depends on whether the device you intend to use
is a target or a sub-target. In either case, the FIDL protocol to use is
[fuchsia.hardware.spmi.Device][spmi.fidl].

### Target device

First update the board driver so that the SPMI controller driver adds a node for
your target. On devicetree platforms, this would involve adding a new node such
as:

```none {:.devsite-disable-click-to-copy}
#include "sdk/lib/driver/devicetree/visitors/drivers/spmi-controllers/spmi/spmi.h"

spmi@0xffff0000 {
    /* The new node is below */
    $TARGET_DEVICE_TYPE@$TARGET_ID {
        reg = <$TARGET_ID SPMI_USID>;
        reg-names = "$TARGET_NAME";
    };
};
```

Your driver should connect to [fuchsia.hardware.spmi.TargetService][spmi.fidl],
and have an entry for it in its component manifest. The SPMI controller driver
will add a node for your target with the following bind properties:

| Property | Optional | Purpose |
| -------- | -------- | ------- |
| `fuchsia.hardware.spmi.TargetService` | N | The SPMI target service. |
| `fuchsia.spmi.TARGET_ID` | N | The SPMI target ID. |
| `fuchsia.spmi.TARGET_NAME` | Y | A string name for the target. On devicetree platforms, this is generated from the target node's `reg-names` property. |

### Sub-target device

First update the board driver so that the SPMI controller driver adds a node for
your sub-target. On devicetree platforms, this would involve adding a new node
such as:

```none {:.devsite-disable-click-to-copy}
#include "sdk/lib/driver/devicetree/visitors/drivers/spmi-controllers/spmi/spmi.h"

spmi@0xffff0000 {
    $TARGET_DEVICE_TYPE@$TARGET_ID {
        reg = <$TARGET_ID SPMI_USID>;
        reg-names = "$TARGET_NAME";

        /* The new node is below */
        $SUB_TARGET_DEVICE_TYPE@$SUB_TARGET_ADDRESS {
            reg = <$SUB_TARGET_ADDRESS $SUB_TARGET_SIZE>;
            reg-names = "$SUB_TARGET_NAME";
        };
    };
};
```

Your driver should connect to
[fuchsia.hardware.spmi.SubTargetService][spmi.fidl], and have an entry for it in
its component manifest. The SPMI controller driver will add a node for your
sub-target with the following bind properties:

| Property | Optional | Purpose |
| -------- | -------- | ------- |
| `fuchsia.hardware.spmi.SubTargetService` | N | The SPMI sub-target service. |
| `fuchsia.spmi.TARGET_ID` | N | The ID of the target that contains this sub-target. |
| `fuchsia.spmi.TARGET_NAME` | Y | A string name for the target that contains this sub-target. On devicetree platforms, this is generated from the target node's `reg-names` property. |
| `fuchsia.spmi.SUB_TARGET_ADDRESS` | N | The starting address for this sub-target within the target. |
| `fuchsia.spmi.SUB_TARGET_NAME` | Y | A string name for the sub-target. On devicetree platforms, this is generated from the sub-target node's `reg-names` property. |

Register accesses through [fuchsia.hardware.spmi.Device][spmi.fidl] will be
mapped to the sub-target's starting address, and limited to the range assigned
to the sub-target.

## Writing an SPMI controller driver

SPMI controller drivers are responsible for setting up child nodes based on
[fuchsia.hardware.spmi.ControllerInfo][metadata.fidl] metadata received from the
board driver.

### Target devices

For each `TargetInfo` in `ControllerInfo` that does not have `sub_targets`, the
controller driver should add a child node that serves
[fuchsia.hardware.spmi.TargetService][spmi.fidl] and has the following
properties:

| Property | Value |
| -------- | ----- |
| `fuchsia.spmi.CONTROLLER_ID` | `id` in `ControllerInfo` (if specified) |
| `fuchsia.spmi.TARGET_ID` | `id` in `TargetInfo` |
| `fuchsia.spmi.TARGET_NAME` | `name` in `TargetInfo` (if specified) |

### Sub-target devices

For each `SubTargetInfo` in each `TargetInfo` in `ControllerInfo`, the
controller driver should add a child node that serves
[fuchsia.hardware.spmi.SubTargetService][spmi.fidl] and has the following
properties:

| Property | Value |
| -------- | ----- |
| `fuchsia.spmi.CONTROLLER_ID` | `id` in `ControllerInfo` (if specified) |
| `fuchsia.spmi.TARGET_ID` | `id` in `TargetInfo` |
| `fuchsia.spmi.TARGET_NAME` | `name` in `TargetInfo` (if specified) |
| `fuchsia.spmi.SUB_TARGET_ADDRESS` | `address` in `SubTargetInfo` |
| `fuchsia.spmi.SUB_TARGET_NAME` | `name` in `SubTargetInfo` (if specified) |

The controller driver should restrict register accesses to sub-targets to the
range [0, `size`) and offset them by `address`.

## Resources

- [SPMI bind rules][spmi.bind]
- [SPMI devicetree visitor][spmi-visitor]
- [SPMI FIDL protocol][spmi.fidl]
- [SPMI metadata][metadata.fidl]

<!-- Reference links -->

[board-driver]: /docs/glossary/README.md#board-driver
[composite-node-spec]: /docs/glossary/README.md#composite-node-specification
[metadata.fidl]: /sdk/fidl/fuchsia.hardware.spmi/metadata.fidl
[spmi.bind]: /src/devices/bind/fuchsia.spmi/fuchsia.spmi.bind
[spmi.fidl]: /sdk/fidl/fuchsia.hardware.spmi/spmi.fidl
[spmi.yaml]: /sdk/lib/driver/devicetree/visitors/drivers/spmi-controllers/spmi/spmi.yaml
[spmi-visitor]: /sdk/lib/driver/devicetree/visitors/drivers/spmi-controllers/spmi
