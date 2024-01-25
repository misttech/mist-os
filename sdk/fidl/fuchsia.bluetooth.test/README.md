# fuchsia.bluetooth.testing Library

This library defines protocols and data structures to drive the behavior of fake
implementations of core Bluetooth system services and hardware protocols.

## [hci_emulator.fidl](./hci_emulator.fidl)

Defines a management interface to create emulated Bluetooth controllers using the
virtual driver (see [bt-hci-virtual](//src/connectivity/bluetooth/hci/virtual)).

## TODO(https://fxbug.dev/42079552): fake_profile.fidl
