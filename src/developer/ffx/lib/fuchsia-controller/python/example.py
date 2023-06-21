# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
from fuchsia_controller_py import Context, IsolateDir
import fidl.fuchsia_developer_ffx as fd_ffx
import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fidl.fuchsia_diagnostics as f_diagnostics
import os
import asyncio


async def echo():
    ctx = Context()
    echo_proxy = fd_ffx.Echo.Client(
        ctx.connect_daemon_protocol(fd_ffx.Echo.MARKER))
    result = await echo_proxy.echo_string(value="foobar")
    print(f"Echo Result: {result}")


async def target_info_isolated():
    isolate = IsolateDir("/tmp/foo/bar")
    ctx = Context({"sdk.root": "."}, isolate_dir=isolate)
    ch = ctx.connect_target_proxy()
    proxy = fd_ffx.Target.Client(ch)
    result = await proxy.identity()
    print(f"Target Info Received: {result}")


async def async_main():
    await echo()
    await target_info_isolated()


def main():
    print("Testing asynchronous calls.")
    asyncio.run(async_main())
    for x in range(5):
        print()
        print(f"Testing synchronous calls, iteration {x}.")
        asyncio.run(echo())
        asyncio.run(target_info_isolated())


if __name__ == "__main__":
    main()
