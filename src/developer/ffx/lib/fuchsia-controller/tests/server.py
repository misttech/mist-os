# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import typing
import unittest

import fidl.fuchsia_controller_othertest as fc_othertest
import fidl.fuchsia_controller_test as fc_test
import fidl.fuchsia_developer_ffx as ffx
import fidl.fuchsia_io as f_io
from fuchsia_controller_py import Channel, ZxStatus

from fidl import DomainError, FrameworkError, StopEventHandler, StopServer


# [START echo_server_impl]
class TestEchoer(ffx.Echo.Server):
    def echo_string(self, request: ffx.EchoEchoStringRequest):
        return ffx.EchoEchoStringResponse(response=request.value)
        # [END echo_server_impl]


class AsyncEchoer(ffx.Echo.Server):
    async def echo_string(self, request: ffx.EchoEchoStringRequest):
        await asyncio.sleep(0.1)  # This isn't necessary, but it is fun.
        return ffx.EchoEchoStringResponse(response=request.value)


class TargetCollectionReaderImpl(ffx.TargetCollectionReader.Server):
    def __init__(self, channel: Channel, target_list):
        super().__init__(channel)
        self.target_list = target_list

    def next(self, request: ffx.TargetCollectionReaderNextRequest):
        if not request.entry:
            raise StopServer
        self.target_list.extend(request.entry)


class TargetCollectionImpl(ffx.TargetCollection.Server):
    async def list_targets(
        self, request: ffx.TargetCollectionListTargetsRequest
    ):
        reader = ffx.TargetCollectionReader.Client(request.reader)
        await reader.next(
            entry=[
                ffx.TargetInfo(nodename="foo"),
                ffx.TargetInfo(nodename="bar"),
            ]
        )
        await reader.next(
            entry=[
                ffx.TargetInfo(nodename="baz"),
            ]
        )
        await reader.next(entry=[])


class StubFileServer(f_io.File.Server):
    def read(self, request: f_io.ReadableReadRequest):
        return f_io.ReadableReadResponse(data=[1, 2, 3, 4])


class TestEventHandler(fc_othertest.CrossLibraryNoop.EventHandler):
    def __init__(
        self,
        client: fc_othertest.CrossLibraryNoop.Client,
        random_event_handler: typing.Callable[
            [fc_othertest.CrossLibraryNoopOnRandomEventRequest], None
        ],
    ):
        super().__init__(client)
        self.random_event_handler = random_event_handler

    def on_random_event(
        self, request: fc_othertest.CrossLibraryNoopOnRandomEventRequest
    ):
        self.random_event_handler(request)

    def on_empty_event(self):
        raise StopEventHandler


class FailingFileServer(f_io.File.Server):
    def read(self, _: f_io.ReadableReadRequest):
        return DomainError(ZxStatus.ZX_ERR_PEER_CLOSED)


class TestingServer(fc_test.Testing.Server):
    def return_union(self):
        res = fc_test.TestingReturnUnionResponse()
        res.y = "foobar"
        return res

    def return_union_with_table(self):
        res = fc_test.TestingReturnUnionWithTableResponse()
        res.y = fc_test.NoopTable(str_="bazzz", integer=-2)
        return res


class ServerTests(unittest.IsolatedAsyncioTestCase):
    async def test_echo_server_sync(self):
        # [START use_echoer_example]
        (tx, rx) = Channel.create()
        server = TestEchoer(rx)
        client = ffx.Echo.Client(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")
        server_task.cancel()
        # [END use_echoer_example]

    async def test_epitaph_propagation(self):
        (tx, rx) = Channel.create()
        client = ffx.Echo.Client(tx)
        coro1 = client.echo_string(value="foobar")
        # Creating a task here so at least one task is awaiting on a staged
        # message/notification.
        task = asyncio.get_running_loop().create_task(
            client.echo_string(value="foobar")
        )
        coro2 = client.echo_string(value="foobar")
        # Put `task` onto the executor so it makes partial progress, since this will yield
        # to the executor.
        await asyncio.sleep(0)
        err_msg = ZxStatus.ZX_ERR_NOT_SUPPORTED
        rx.close_with_epitaph(err_msg)

        # The main thing here is to ensure that PEER_CLOSED is not sent early.
        # After running rx.close_with_epitaph, the channel will be closed, and
        # that message will have been queued for the client.
        with self.assertRaises(ZxStatus):
            try:
                await coro1
            except ZxStatus as e:
                self.assertEqual(e.args[0], err_msg)
                raise e

        with self.assertRaises(ZxStatus):
            try:
                await task
            except ZxStatus as e:
                self.assertEqual(e.args[0], err_msg)
                raise e

        with self.assertRaises(ZxStatus):
            try:
                await coro2
            except ZxStatus as e:
                self.assertEqual(e.args[0], err_msg)
                raise e

        # Finally, ensure that the channel is just plain-old closed for new
        # interactions.
        with self.assertRaises(ZxStatus):
            try:
                await client.echo_string(value="foobar")
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    async def test_echo_server_async(self):
        (tx, rx) = Channel.create()
        server = AsyncEchoer(rx)
        client = ffx.Echo.Client(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")
        server_task.cancel()

    # TODO(https://fxbug.dev/364878315): This test sends many messages to AsyncEchoer which causes
    # many spurious wakeups as a side-effect. Having this test ensures ServerBase is resilient
    # to spurious wakeups, even several at a time.
    async def test_echo_server_async_stress(self):
        (tx, rx) = Channel.create()
        server = AsyncEchoer(rx)
        client = ffx.Echo.Client(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        for _ in range(12):
            res = await client.echo_string(value="foobar")
            self.assertEqual(res.response, "foobar")
        server_task.cancel()

    async def test_not_implemented(self):
        (tx, rx) = Channel.create()
        server = ffx.Echo.Server(rx)
        client = ffx.Echo.Client(tx)
        task = asyncio.get_running_loop().create_task(server.serve())
        # The first thing that will happen is the server will receive the
        # request, then attempt to call the corresponding function. Since it is not implemented,
        # An exception will be raised, closing the channel. The client will then receive a
        # PEER_CLOSED error. In order to diagnose the root cause the task must then be awaited.
        with self.assertRaises(ZxStatus):
            try:
                res = await client.echo_string(value="foobar")
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e
        self.assertTrue(task.done())
        with self.assertRaises(NotImplementedError):
            await task

    async def test_target_iterator(self):
        (reader_client_channel, reader_server_channel) = Channel.create()
        target_list: typing.List[typing.Any] = []
        server = TargetCollectionReaderImpl(reader_server_channel, target_list)
        (tc_client_channel, tc_server_channel) = Channel.create()
        target_collection_server = TargetCollectionImpl(tc_server_channel)
        loop = asyncio.get_running_loop()
        reader_task = loop.create_task(server.serve())
        tc_task = loop.create_task(target_collection_server.serve())
        tc_client = ffx.TargetCollection.Client(tc_client_channel)
        tc_client.list_targets(
            query=ffx.TargetQuery(), reader=reader_client_channel.take()
        )
        done, pending = await asyncio.wait(
            [reader_task, tc_task], return_when=asyncio.FIRST_COMPLETED
        )
        # This will just surface exceptions if they happen. For correct behavior this should just
        # return the result of the reader task.
        done.pop().result()
        self.assertEqual(len(target_list), 3)
        foo_targets = [x for x in target_list if x.nodename == "foo"]
        self.assertEqual(len(foo_targets), 1)
        bar_targets = [x for x in target_list if x.nodename == "bar"]
        self.assertEqual(len(bar_targets), 1)
        baz_targets = [x for x in target_list if x.nodename == "baz"]
        self.assertEqual(len(baz_targets), 1)

    async def test_file_server(self):
        # This handles the kind of case where a method has a signature of `-> (data) error Error;`
        client, server = Channel.create()
        file_proxy = f_io.File.Client(client)
        file_server = StubFileServer(server)
        server_task = asyncio.get_running_loop().create_task(
            file_server.serve()
        )
        res = await file_proxy.read(count=4)
        self.assertEqual(res.response.data, [1, 2, 3, 4])
        server_task.cancel()

    async def test_failing_file_server(self):
        client, server = Channel.create()
        file_proxy = f_io.File.Client(client)
        file_server = FailingFileServer(server)
        server_task = asyncio.get_running_loop().create_task(
            file_server.serve()
        )
        res = await file_proxy.read(count=4)
        self.assertEqual(res.response, None)
        self.assertEqual(res.err, ZxStatus.ZX_ERR_PEER_CLOSED)
        server_task.cancel()

    async def test_testing_server(self):
        client, server = Channel.create()
        t_client = fc_test.Testing.Client(client)
        t_server = TestingServer(server)
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res = await t_client.return_union()
        self.assertEqual(res.y, "foobar")
        res = await t_client.return_union_with_table()
        self.assertEqual(res.y.str_, "bazzz")
        self.assertEqual(res.y.integer, -2)
        server_task.cancel()

    async def test_flexible_method_framework_err(self):
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTester.Server):
            def some_method(self):
                # This should be handled internally, but right now there's not really
                # a good way to force this interaction without making multiple FIDL
                # versions run in this program simultaneously somehow.
                return FrameworkError.UNKNOWN_METHOD

            def some_method_without_error(self):
                return FrameworkError.UNKNOWN_METHOD

            def some_method_just_error(self):
                return FrameworkError.UNKNOWN_METHOD

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTester.Client(client)
        t_server = FlexibleMethodTesterServer(server)
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res = await t_client.some_method()
        self.assertEqual(res.framework_err, FrameworkError.UNKNOWN_METHOD)
        res = await t_client.some_method_without_error()
        self.assertEqual(res.framework_err, FrameworkError.UNKNOWN_METHOD)
        res = await t_client.some_method_just_error()
        self.assertEqual(res.framework_err, FrameworkError.UNKNOWN_METHOD)
        server_task.cancel()

    async def test_flexible_method_unwrap_response(self):
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTester.Server):
            def some_method(self):
                return fc_test.FlexibleMethodTesterSomeMethodResponse(
                    some_bool_value=True
                )

            def some_method_without_error(self):
                return (
                    fc_test.FlexibleMethodTesterSomeMethodWithoutErrorResponse(
                        some_bool_value=False
                    )
                )

            def some_method_just_error(self):
                return fc_test.FlexibleMethodTesterSomeMethodJustErrorResponse()

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTester.Client(client)
        t_server = FlexibleMethodTesterServer(server)
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res = await t_client.some_method()
        self.assertTrue(res.unwrap().some_bool_value)
        res = await t_client.some_method_without_error()
        self.assertFalse(res.unwrap().some_bool_value)
        res = await t_client.some_method_just_error()
        self.assertEqual(
            res.unwrap(),
            fc_test.FlexibleMethodTesterSomeMethodJustErrorResponse(),
        )
        server_task.cancel()

    async def test_flexible_method_unwrap_error(self):
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTester.Server):
            def some_method(self):
                return DomainError(error=ZxStatus.ZX_ERR_INTERNAL)

            def some_method_without_error(self):
                return FrameworkError.UNKNOWN_METHOD

            def some_method_just_error(self):
                return DomainError(error=ZxStatus.ZX_ERR_INTERNAL)

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTester.Client(client)
        t_server = FlexibleMethodTesterServer(server)
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res = await t_client.some_method()
        with self.assertRaisesRegex(RuntimeError, "Result error"):
            res.unwrap()
        res = await t_client.some_method_without_error()
        with self.assertRaisesRegex(RuntimeError, "Result framework error"):
            res.unwrap()
        res = await t_client.some_method_just_error()
        with self.assertRaisesRegex(RuntimeError, "Result error"):
            res.unwrap()
        server_task.cancel()

    async def test_sending_and_receiving_event(self):
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoop.Client(client)
        THIS_EXPECTED = 3
        THAT_EXPECTED = fc_othertest.TestingEnum.FLIPPED_OTHER_TEST

        def random_event_handler(
            request: fc_othertest.CrossLibraryNoopOnRandomEventRequest,
        ):
            self.assertEqual(request.this, THIS_EXPECTED)
            self.assertEqual(request.that, THAT_EXPECTED)
            raise StopEventHandler

        # It's okay to use an unimplemented server here since we're not fielding any calls.
        t_server = fc_othertest.CrossLibraryNoop.Server(server)
        event_handler = TestEventHandler(t_client, random_event_handler)
        t_server.on_random_event(this=THIS_EXPECTED, that=THAT_EXPECTED)
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        # If the event is never received this will loop forever (since the channel is open, and the
        # handler should stop service after receiving the event.
        await task

    async def test_sending_and_receiving_empty_event(self):
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoop.Client(client)
        # It's okay to use an unimplemented server here since we're not fielding any calls.
        t_server = fc_othertest.CrossLibraryNoop.Server(server)
        event_handler = TestEventHandler(t_client, self)
        t_server.on_empty_event()
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        # If the event is never received this will loop forever (since the channel is open, and the
        # handler should stop service after receiving the event.
        await task

    async def test_closing_channel_closes_event_loop(self):
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoop.Client(client)
        del server
        # A generic unimplemented event handler is fine, since we're just making it exit.
        event_handler = fc_othertest.CrossLibraryNoop.EventHandler(t_client)
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        await task

    async def test_echo_server_unimplemented(self):
        _client, server = Channel.create()
        s = fc_test.Noop.Server(server)
        with self.assertRaises(NotImplementedError):
            s.do_noop()
