# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
from typing import Awaitable
from typing import Callable
from typing import List

import fidl.fuchsia_developer_ffx as ffx
import fidl.fuchsia_device as device
import fidl.fuchsia_feedback as feedback
import fidl.fuchsia_io as io
from fuchsia_controller_py import Channel
from fuchsia_controller_py import ZxStatus
from mobly import asserts
from mobly import base_test
from mobly import test_runner

from mobly_controller import fuchsia_device


def asynctest(func: Callable[[base_test.BaseTestClass], Awaitable[None]]):
    """Simple wrapper around async tests.

    Args:
        func: The test which is being wrapped.

    Returns:
        The wrapped function. Runs the body of the `func` in asyncio.run()
    """

    def wrapper(*args, **kwargs):
        coro = func(*args, **kwargs)
        asyncio.run(coro)

    return wrapper


class FuchsiaControllerTests(base_test.BaseTestClass):

    def setup_class(self) -> None:
        self.fuchsia_devices: List[
            fuchsia_device.FuchsiaDevice] = self.register_controller(
                fuchsia_device)
        self.device = self.fuchsia_devices[0]

    @asynctest
    async def test_get_nodename(self) -> None:
        """Test that gets the nodename using the Target daemon protocol.

        Targeted criteria:
        -- call that results in simple output.
        -- calls a non-SDK protocol (daemon protocols are not explicitly in the SDK).
        """
        target_proxy = ffx.Target.Client(self.device.ctx.connect_target_proxy())
        res = await target_proxy.identity()
        target_info = res.target_info
        asserts.assert_equal(target_info.nodename, self.device.config["name"])

    @asynctest
    async def test_get_device_info(self) -> None:
        """Gets target device info and compares to internal fuchsia_device config.

        Targeted criteria:
        -- calls a FIDL API on the Fuchsia target from the host.
        -- call that results in simple output.
        """
        device_proxy = device.NameProvider.Client(
            self.device.ctx.connect_device_proxy(
                "/bootstrap/device_name_provider", device.NameProvider.MARKER))
        res = await device_proxy.get_device_name()
        asserts.assert_not_equal(res.response, None, extras=res)
        name = res.response.name
        asserts.assert_equal(name, self.device.config["name"])

    @asynctest
    async def test_get_fuchsia_snapshot(self) -> None:
        """Gets Fuchsia Snapshot info.

        Targeted criteria:
        -- call that results in streamed output (though via fuchsia.io/File protocol and not a
        socket).
        -- call that takes another FIDL protocol's channel as a parameter (fuchsia.io/File server).
        """
        client, server = Channel.create()
        file = io.File.Client(client)
        params = feedback.GetSnapshotParameters(
            # Two minutes of timeout time.
            collection_timeout_per_data=(2 * 60 * 10**9),
            response_channel=server.take())
        ch = self.device.ctx.connect_device_proxy(
            "/core/feedback", "fuchsia.feedback.DataProvider")
        provider = feedback.DataProvider.Client(ch)
        await provider.get_snapshot(params=params)
        attr_res = await file.get_attr()
        asserts.assert_equal(attr_res.s, ZxStatus.ZX_OK)
        data = bytearray()
        while True:
            response = await file.read(count=io.MAX_BUF)
            asserts.assert_not_equal(response.response, None, extras=response)
            response = response.response
            if not response.data:
                break
            data.extend(response.data)
        asserts.assert_equal(len(data), attr_res.attributes.content_size)


if __name__ == "__main__":
    test_runner.main()
