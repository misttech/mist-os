# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import logging

import fidl.fuchsia_developer_ffx as fd_ffx
from fuchsia_controller_py import Context, IsolateDir

logging.basicConfig(level=logging.DEBUG)


async def echo() -> None:
    ctx = Context()
    echo_proxy = fd_ffx.Echo.Client(
        ctx.connect_daemon_protocol(fd_ffx.Echo.MARKER)
    )
    result = await echo_proxy.echo_string(value="foobar")
    print(f"Echo Result: {result}")


async def target_info_multi_target_isolated() -> None:
    isolate = IsolateDir()  # Will create a random tmpdir
    ctx = Context(
        config={"sdk.root": "."}, isolate_dir=isolate, target="fuchsia-emulator"
    )
    ctx2 = Context(
        config={"sdk.root": "."}, isolate_dir=isolate, target="emu-two"
    )
    ch = ctx.connect_target_proxy()
    proxy = fd_ffx.Target.Client(ch)
    proxy2 = fd_ffx.Target.Client(ctx2.connect_target_proxy())
    result2 = proxy2.identity()
    result1 = proxy.identity()
    results = await asyncio.gather(*[result1, result2])
    print(f"Target Info Received: {results}")


async def multi_echo() -> None:
    ctx = Context()
    echo_proxy = fd_ffx.Echo.Client(
        ctx.connect_daemon_protocol(fd_ffx.Echo.MARKER)
    )
    result1 = echo_proxy.echo_string(value="1foobington")
    result2 = echo_proxy.echo_string(value="2frobination")
    result3 = echo_proxy.echo_string(value="3frobinationator")
    result4 = echo_proxy.echo_string(value="4frobinationatorawoihoiwf")
    result5 = echo_proxy.echo_string(value="5frobin")
    results = await asyncio.gather(result1, result2, result3, result4, result5)
    print(f"Multi-echo results: {results}")


async def async_main() -> None:
    await echo()
    await multi_echo()
    await target_info_multi_target_isolated()


def main() -> None:
    print("Testing asynchronous calls.")
    asyncio.run(async_main())
    for x in range(5):
        print()
        print(f"Testing synchronous calls, iteration {x}.")
        asyncio.run(echo())
        asyncio.run(multi_echo())
        asyncio.run(target_info_multi_target_isolated())


if __name__ == "__main__":
    main()
