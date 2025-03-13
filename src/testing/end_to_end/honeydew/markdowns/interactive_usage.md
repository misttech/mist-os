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

* Run appropriate `fx set` command along with
`--with-host //src/testing/end_to_end/honeydew` param.
  * If test case requires `SL4f` transport then make sure to do the following:
    * Use a `<PRODUCT>` that supports `SL4F` (such as `core`)
    * Include `--with //src/testing/sl4f --with //src/sys/bin/start_sl4f`
    * Include `--args=core_realm_shards+="[\"//src/testing/sl4f:sl4f_core_shard\"]"`
      * Alternatively, run `fx set` without the `--args` option, and then run
      `fx args`, and add the line directly to the `args.gn` file in the editor
      that opens.
* Run `fx build`
```shell
~/fuchsia$ fx set core.x64 --with-host //src/testing/end_to_end/honeydew
~/fuchsia$ fx build
```

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
>>> FFX_PLUGINS_PATH=f"{FUCHSIA_ROOT}/out/default/host-tools"
>>> from honeydew.transports.ffx import ffx_impl
>>> ffx_config = ffx_impl.FfxConfig()
>>> ffx_config.setup(binary_path=FFX_BIN, isolate_dir=None, logs_dir="/tmp/logs/honeydew/", logs_level="debug", enable_mdns=True, subtools_search_path=FFX_PLUGINS_PATH, proxy_timeout_secs=60)

>>> import honeydew
>>> from honeydew.typing import custom_types

# Create Honeydew device object for a local device
>>> device_info = custom_types.DeviceInfo(name="fuchsia-emulator", ip_port=None, serial_socket=None)
>>> emu = honeydew.create_device(device_info, ffx_config=ffx_config.get_config())

# Create Honeydew device object for a remote/wfh device
# "[::1]:8022" is a fuchsia device whose SSH port is proxied via SSH from a local machine to a remote workstation.
>>> device_ip_port=custom_types.IpPort.create_using_ip_and_port("[::1]:8022")
>>> device_info = custom_types.DeviceInfo(name="fuchsia-d88c-796c-e57e", ip_port=device_ip_port, serial_socket=None)
>>> fd_remote = honeydew.create_device(device_info, ffx_config=ffx_config.get_config())

# Note - If any of the affordance you plan to use need a specific config, then you need to pass this information in `config` argument of `create_device()`. Refer to `create_device()` documentation for more information.

# You can now start doing host-(fuchsia)target interactions using object returned by `honeydew.create_device()`
# To check all operations, affordances and transports supported, use `dir` command
>>> dir(emu)
```
