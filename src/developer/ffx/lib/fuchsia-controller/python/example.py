# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
from fuchsia_controller_py import Context
import fidl.fuchsia_developer_ffx
import os
import asyncio


async def echo():
    ctx = Context({})
    ch = ctx.open_target_proxy()
    echo_proxy = fidl.fuchsia_developer_ffx.Echo.Client(
        ctx.open_daemon_protocol('fuchsia.developer.ffx.Echo'))
    result = await echo_proxy.echo_string(value="foobar")
    print(f"Echo Result: {result}")


async def target_info():
    ctx = Context({})
    ch = ctx.open_target_proxy()
    proxy = fidl.fuchsia_developer_ffx.Target.Client(ch)
    result = await proxy.identity()
    print(f"Target Info Received: {result}")


async def async_main():
    # TODO(fxbug.dev/128817): This should be handled automatically by the build system, or by some
    # mechanism other than the user manually exporting it (locating the ffx binary should be
    # mostly automatic).
    os.putenv("FFX_BIN", "host_x64/ffx")
    await target_info()
    await echo()


def main():
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
