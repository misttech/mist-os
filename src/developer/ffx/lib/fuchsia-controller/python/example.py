# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
from fuchsia_controller_py import Context, open_handle_notifier
import os
import asyncio


async def channel_read(fd, channel):
    while True:
        try:
            return channel.read()
        except RuntimeError:
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(None, lambda: os.read(fd, 4))


async def async_main():
    os.putenv("FFX_BIN", "host_x64/ffx")
    ctx = Context({})
    fd = open_handle_notifier()
    ch = ctx.open_daemon_protocol("fuchsia.developer.ffx.Echo")
    # For now this is just a raw encoded FIDL message.
    out = bytearray(
        b"\x01\x00\x00\x00\x02\x00\x00\x01%z\xcet\x90g/\x00\x06\x00\x00\x00\x00\x00\x00\x00\xff\xff\xff\xff\xff\xff\xff\xfffoobar\x00\x00"
    )
    ch.write((out, []))
    print(f"Was able to write: {out}")
    (b, _) = await channel_read(fd, ch)
    print(f"Read: {b}")
    assert out == b


def main():
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
