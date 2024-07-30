# `shutdown-shim`

`shutdown-shim` is load-bearing piece of system-wide shutdown and reboot
procedures. All components that start a reboot or shutdown do so by talking to
`shutdown-shim` via `fuchsia.hardware.power.statecontrol.Admin`.

`shutdown-shim` does the following:

-   It implements `fuchsia.hardware.power.statecontrol.Admin` by forwarding
    calls to `power-manager`, falling back to a forced shutdown in the event of
    a transport error. This provides limited fault tolerance for the shutdown
    process.
-   It prevents suspension during the shutdown process.
-   It eliminates a dependency cycle between drivers and `power-manager` by
    implementing `fuchsia.device.manager.SystemStateTransition`.
