# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
from fuchsia_controller_py import Context, open_handle_notifier
from fidl_codec import encode_fidl_message, decode_fidl_response
import os
import asyncio


async def channel_read(fd, channel):
    while True:
        try:
            return channel.read()
        except RuntimeError:
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(None, lambda: os.read(fd, 4))


class EchoRequest(object):

    def __init__(self, s):
        self.value = s


async def async_main():
    os.putenv("FFX_BIN", "host_x64/ffx")
    ctx = Context({})
    fd = open_handle_notifier()
    ch = ctx.open_daemon_protocol("fuchsia.developer.ffx.Echo")
    out = encode_fidl_message(
        object=EchoRequest("foobar"),
        library="fuchsia.developer.ffx",
        type_name="fuchsia.developer.ffx/EchoEchoStringRequest",
        txid=1,
        ordinal=13343194038041125,
    )
    ch.write(out)
    print(f"Was able to write: {out[0]}")
    (b, hdls) = await channel_read(fd, ch)
    print(f"Read: {b}")
    assert out[0] == b
    result = decode_fidl_response(bytes=b, handles=hdls)
    print(f"Decoded: {result}")
    assert result == {"response": "foobar"}


def main():
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
