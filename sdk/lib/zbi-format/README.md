# `zbi-format` Library

This library contains the definitions for the Zircon Boot Image (ZBI) format.

## Maintenance & Evolution

### ABI Stability

The ZBI definitions form the contract between a bootloader and the kernel so
must remain ABI backwards-compatible. Failure to do so would result in undefined
behavior when devices update their kernel to the new ABI but their bootloader
still uses the old ABI (or vice-versa).

It is generally infeasible to update all bootloaders and kernels to a new ABI in
lockstep, not only due to the proliferation of boards running Fuchsia, but also
because many boards do not support firmware A/B/R without which it is impossible
to guarantee atomic updates to both the bootloader and kernel.

### API Stability

API stability is not required. These definitions are not exported to Fuchsia
application developers so will not break any out-of-tree builds if e.g. a
constant name is updated.

They are exported in the Firmware SDK for firmware developers, but firmware
does not expect to automatically roll new SDK builds; any uprev to a new SDK
is an intentional action that expects some porting work.
