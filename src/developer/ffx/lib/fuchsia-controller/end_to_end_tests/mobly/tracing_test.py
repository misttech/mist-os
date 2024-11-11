# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import json
import subprocess

import fidl.fuchsia_tracing as tracing
import fidl.fuchsia_tracing_controller as tracing_controller
from fuchsia_controller_py import Channel, Socket
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from mobly import asserts, base_test, test_runner
from mobly_controller import fuchsia_device

from fidl import AsyncSocket

TRACE2JSON = "tracing_runtime_deps/trace2json"


class FuchsiaControllerTests(AsyncAdapter, base_test.BaseTestClass):
    def setup_class(self) -> None:
        self.fuchsia_devices: list[
            fuchsia_device.FuchsiaDevice
        ] = self.register_controller(fuchsia_device)
        self.device = self.fuchsia_devices[0]
        self.device.set_ctx(self)

    @asyncmethod
    async def test_fuchsia_device_get_known_categories(self) -> None:
        """Verifies that kernel:vm is an existing category for tracing on the device."""
        if self.device.ctx is None:
            raise ValueError(f"Device: {self.device.target} has no context")
        ch = self.device.ctx.connect_device_proxy(
            "core/trace_manager", tracing_controller.Provisioner.MARKER
        )
        controller = tracing_controller.Provisioner.Client(ch)
        res = await controller.get_known_categories()
        asserts.assert_true(
            res.response,
            msg="Error retrieving known categories",
        )
        categories = res.response.categories
        found_kernel_category = False
        for category in categories:
            if category.name == "kernel:vm":
                found_kernel_category = True
                break
        asserts.assert_true(
            found_kernel_category,
            msg="Was not able to find 'kernel.vm' category in known output",
        )

    @asyncmethod
    async def test_fuchsia_device_tracing_start_stop(self) -> None:
        """Does a simple start and stop of tracing on a device."""
        if self.device.ctx is None:
            raise ValueError(f"Device: {self.device.target} has no context")
        ch = self.device.ctx.connect_device_proxy(
            "core/trace_manager", tracing_controller.Provisioner.MARKER
        )
        provisioner = tracing_controller.Provisioner.Client(ch)
        categories = [
            "blobfs",
            "gfx",
            "system_metrics",
        ]
        config = tracing_controller.TraceConfig(
            buffer_size_megabytes_hint=4,
            categories=categories,
            buffering_mode=tracing.BufferingMode.ONESHOT,
        )
        _client, server = Socket.create()
        client = AsyncSocket(_client)

        client_end, server_end = Channel.create()

        provisioner.initialize_tracing(
            controller=server_end.take(), config=config, output=server.take()
        )
        controller = tracing_controller.Session.Client(client_end)

        await controller.start_tracing()
        socket_task = asyncio.get_running_loop().create_task(client.read_all())
        await asyncio.sleep(10)
        stop_res = await controller.stop_tracing(write_results=True)
        asserts.assert_true(
            stop_res.response,
            msg="Error stopping tracing",
        )

        stop_result = stop_res.response
        asserts.assert_true(
            len(stop_result.provider_stats) > 0,
            msg="Stop result provider stats should not be empty.",
        )

        # Closing the channel will terminate tracing
        controller.channel.close()

        raw_trace = await socket_task
        asserts.assert_equal(type(raw_trace), bytearray)
        asserts.assert_true(
            len(raw_trace) > 0, msg="Output bytes should not be empty."
        )
        ps = subprocess.Popen(
            [TRACE2JSON], stdin=subprocess.PIPE, stdout=subprocess.PIPE
        )
        js, _ = ps.communicate(input=raw_trace)
        js_obj = json.loads(js.decode("utf8"))
        ps.kill()
        asserts.assert_true(
            js_obj.get("traceEvents") is not None,
            "Expected traceEvents to be present",
        )
        for trace_event in js_obj["traceEvents"]:
            trace_cat = trace_event["cat"]
            asserts.assert_true(
                trace_cat in categories,
                msg=f"Found unexpected category that isn't part of trace: {trace_cat}",
            )


if __name__ == "__main__":
    test_runner.main()
