# Fake Display Driver Stack

`FakeDisplayStack` is a series of drivers provided as standalone device nodes
(device tree) independent from the system display coordinator and display
engine drivers.

This includes a [fake display][fake-display] driver, a fake display
[coordinator][display-coordinator] driver and a fake [sysmem][sysmem] Allocator
driver.

## Fake display driver

`fake-display` is a display engine driver that allows clients to import and
present images and capture the current displayed contents.

The `fake-display` driver exports structured display configuration and state
using the [Inspect API][inspect] only for developer debugging purpose. Tests
must not depend on the inspection contents from driver host or components
providing `fake-display`.

[fake-display]://src/graphics/display/drivers/fake/
[display-coordinator]://src/graphics/display/drivers/coordinator/
[sysmem]://src/devices/sysmem/drivers/sysmem/
[inspect]://docs/development/diagnostics/inspect/README.md
