# Interactive Usage

[TOC]

This page will guide you to use Honeydew using an interactive python terminal.

Before proceeding further, please make sure to follow [Setup](#Setup) and
[Installation](code_guidelines.md#installation) steps.

After the installation succeeds, follow the script's instruction message to
start a Python interpreter and import Honeydew.

## Setup
The Honeydew library depends on some Fuchsia build artifacts that must be built
for Honeydew to be successfully imported in Python. So before running
[conformance scripts](../README.md#honeydew-code-guidelines) or to use Honeydew
in interactive python terminal, you need to run the below commands.

```shell
~/fuchsia$ fx set core.qemu-x64 \
--with-host //src/testing/end_to_end/honeydew \
--with //src/testing/sl4f --with //src/sys/bin/start_sl4f \
--args='core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'

~/fuchsia$ fx build
```

* The `core.qemu-x64` product config can be updated out to match the
`product.board` combination you want to test against.
* `--with-host //src/testing/end_to_end/honeydew` is required so that
Honeydew dependencies are built (e.g. Fuchsia controller's shared libraries).
* (Optional) `--with //src/testing/sl4f --with //src/sys/bin/start_sl4f \
--args='core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'` is only
required if you wish to use Honeydew's SL4F-based affordances.

## Creation
```python
# Enable Info logging
>>> import logging
>>> logging.basicConfig(level=logging.INFO)

# Setup Honeydew to run using isolated FFX and collect the logs
# Call this first prior to calling any other Honeydew API
>>> import os
>>> FUCHSIA_ROOT = os.environ.get("FUCHSIA_DIR")
>>> FFX_BIN = f"{FUCHSIA_ROOT}/.jiri_root/bin/ffx"
>>> from honeydew.transports import ffx
>>> ffx_config = ffx.FfxConfig()
>>> ffx_config.setup(binary_path=FFX_BIN, isolate_dir=None, logs_dir="/tmp/logs/honeydew/", logs_level="debug", enable_mdns=True)

# Create Honeydew device object for a local device
>>> import honeydew
emu = honeydew.create_device("fuchsia-emulator", transport=honeydew.transports.TRANSPORT.SL4F, ffx_config=ffx_config.get_config())
# Note - Depending on whether you want to use SL4F or Fuchsia-Controller as a primary transport to perform the host-(fuchsia) target communications, set `transport` variable accordingly

# Create Honeydew device object for a remote/wfh device
>>> from honeydew.typing import custom_types
# Note - While using remote/wfh device, you need to pass `device_ip_port` argument.
# "[::1]:8022" is a fuchsia device whose SSH port is proxied via SSH from a local machine to a remote workstation.
>>> fd_remote = honeydew.create_device("fuchsia-d88c-796c-e57e", transport=honeydew.transports.TRANSPORT.SL4F, ffx_config=ffx_config.get_config(), device_ip_port=custom_types.IpPort.create_using_ip_and_port("[::1]:8022"))

# You can now start doing host-(fuchsia)target interactions using object returned by `honeydew.create_device()`
# To check all operations, affordances and transports supported, use `dir` command
>>> dir(emu)
```
