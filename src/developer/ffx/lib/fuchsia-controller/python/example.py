# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import argparse
import asyncio
import logging
import pprint

import fidl.fuchsia_developer_remotecontrol as fd_rcs
from fuchsia_controller_py import Context

logging.basicConfig(level=logging.DEBUG)

USAGE = "This script prints the result of RemoteControl.IdentifyHost on Fuchsia targets."


async def main() -> None:
    params = argparse.ArgumentParser(usage=USAGE)
    params.add_argument(
        "target_ips",
        help="Fuchsia target IP addresses",
        nargs="+",
    )
    args = params.parse_args()

    # Return the result of RemoteControl.IdentifyHost on a Fuchsia target.
    async def identify_host(target_ip: str) -> str:
        ctx = Context(
            config={"sdk.root": "."},
            target=target_ip,
        )
        remote_control_proxy = ctx.connect_remote_control_proxy()
        proxy = fd_rcs.RemoteControlClient(remote_control_proxy)
        return await proxy.identify_host()

    # Asynchronously await each RemoteControl.IdentifyHost call.
    results = await asyncio.gather(*map(identify_host, args.target_ips))

    print("Target Info Received:")
    pprint.pp(results)


if __name__ == "__main__":
    asyncio.run(main())
