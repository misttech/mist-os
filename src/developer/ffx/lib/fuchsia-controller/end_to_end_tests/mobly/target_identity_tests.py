# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
from collections import deque
from typing import Any, List

import fidl_fuchsia_device as device
import fidl_fuchsia_feedback as feedback
import fidl_fuchsia_hwinfo as hwinfo
import fidl_fuchsia_io as io
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from mobly import asserts, base_test, test_runner
from mobly_controller import fuchsia_device


class FuchsiaControllerTests(AsyncAdapter, base_test.BaseTestClass):
    def setup_class(self) -> None:
        self.fuchsia_devices: List[
            fuchsia_device.FuchsiaDevice
        ] = self.register_controller(fuchsia_device)
        self.device = self.fuchsia_devices[0]
        self.device.set_ctx(self)

    @asyncmethod
    async def test_get_device_info(self) -> None:
        """Gets target device info, compares to internal fuchsia_device config.

        Targeted criteria:
        -- calls a FIDL API on the Fuchsia target from the host.
        -- call that results in simple output.
        """
        assert self.device.ctx is not None
        device_proxy = device.NameProviderClient(
            self.device.ctx.connect_device_proxy(
                "/bootstrap/device_name_provider", device.NameProviderMarker
            )
        )
        name = (await device_proxy.get_device_name()).unwrap().name
        asserts.assert_equal(name, self.device.config["name"])

    @asyncmethod
    async def test_get_device_info_and_hwinfo(self) -> None:
        """Gets two different component infos in parallel.

        Targeted criteria:
        -- calls a FIDL API on the Fuchsia target from the host.
        -- calls multiple different components in a single test.
        """
        assert self.device.ctx is not None
        device_proxy = device.NameProviderClient(
            self.device.ctx.connect_device_proxy(
                "/bootstrap/device_name_provider", device.NameProviderMarker
            )
        )
        product_proxy = hwinfo.ProductClient(
            self.device.ctx.connect_device_proxy(
                "/core/hwinfo", hwinfo.ProductMarker
            )
        )
        loop = asyncio.get_running_loop()
        tasks = [
            loop.create_task(device_proxy.get_device_name()),
            loop.create_task(product_proxy.get_info()),
        ]
        results: deque[Any] = deque([])
        # The tasks will still happen in parallel as the await
        # continues, but they will return in strict order.
        for task in tasks:
            results.append(await task)
        device_name_res = results.popleft()
        product_res = results.popleft().info
        asserts.assert_not_equal(product_res, None, extras=str(product_res))
        asserts.assert_not_equal(
            device_name_res.response, None, extras=str(device_name_res)
        )
        name = device_name_res.response.name
        asserts.assert_equal(name, self.device.config["name"])

        expected_values = self.user_params["expected_values"]
        asserts.assert_equal(
            product_res.model, expected_values["model"], extras=str(product_res)
        )
        asserts.assert_equal(
            product_res.manufacturer,
            expected_values["manufacturer"],
            extras=str(product_res),
        )

    @asyncmethod
    async def test_get_fuchsia_snapshot(self) -> None:
        """Gets Fuchsia Snapshot info.

        Targeted criteria:
        -- call that results in streamed output (though via fuchsia.io/File
           protocol and not a socket).
        -- call that takes another FIDL protocol's channel as a parameter
           (fuchsia.io/File server).
        """
        # [START snapshot_example]
        client, server = Channel.create()
        file = io.FileClient(client)
        params = feedback.GetSnapshotParameters(
            # Two minutes of timeout time.
            collection_timeout_per_data=(2 * 60 * 10**9),
            response_channel=server.take(),
        )
        assert self.device.ctx is not None
        ch = self.device.ctx.connect_device_proxy(
            "/core/feedback", "fuchsia.feedback.DataProvider"
        )
        provider = feedback.DataProviderClient(ch)
        await provider.get_snapshot(params=params)
        attr_res = await file.get_attr()
        asserts.assert_equal(attr_res.s, ZxStatus.ZX_OK)
        data = bytearray()
        while True:
            response = (await file.read(count=io.MAX_BUF)).unwrap()
            if not response.data:
                break
            data.extend(response.data)
        # [END snapshot_example]
        asserts.assert_equal(len(data), attr_res.attributes.content_size)


if __name__ == "__main__":
    test_runner.main()
