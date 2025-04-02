# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import inspect
import typing
import unittest
from typing import Any

import fidl_fuchsia_controller_othertest as fc_othertest
import fidl_fuchsia_controller_test as fc_test
import fidl_fuchsia_developer_ffx as ffx
import fidl_fuchsia_io as f_io
from fidl import DomainError, FrameworkError, StopEventHandler, StopServer
from fuchsia_controller_py import Channel, ZxStatus

T = typing.TypeVar("T")


# TODO(https://fxbug.dev/407643032): Extract to separate library and add unit tests.
def implement_missing_abstract_methods(cls: type[T]) -> type[T]:
    """
    Class decorator to implement missing abstract methods to satisfy ABC requirements for stubbing.

    Identifies abstract methods defined in base classes (using MRO and __abstractmethods__) that are
    not implemented directly in the decorated class and adds a default implementation that raises
    NotImplementedError.

    Note: This only provides *runtime* implementations. Mypy will still require
    `# type: ignore[abstract]` at locations where a class is instantiated.
    """
    if not inspect.isclass(cls):
        raise TypeError("This decorator can only be used on classes.")

    # Collect all unique abstract method names from the MRO (Method Resolution Order)
    abstract_method_names: set[str] = set()
    for base in cls.__mro__:
        if hasattr(base, "__abstractmethods__") and isinstance(
            base.__abstractmethods__, frozenset
        ):
            abstract_method_names.update(base.__abstractmethods__)

    implemented_members = set(cls.__dict__.keys())
    pre_missing_methods = abstract_method_names - implemented_members

    # First pass to check if each method is actually abstract *at this point* in the MRO. This
    # avoids issues when the MRO is complex, i.e., there are both abstract and concrete method
    # implementations in the MRO.
    missing_methods: list[str] = []
    for name in pre_missing_methods:
        try:
            member = getattr(cls, name)
            if not getattr(member, "__isabstractmethod__", False):
                continue
        except AttributeError:
            pass
        missing_methods.append(name)

    # Final pass to implement each method by raising a verbose NotImplementedError
    for name in missing_methods:

        def make_default_impl(method_name: str) -> typing.Callable[..., Any]:
            def default_impl(*args: list[Any], **kwargs: dict[str, Any]) -> Any:
                raise NotImplementedError(
                    f"Abstract method '{method_name}' was auto-implemented "
                    f"in {cls.__name__} and called without override."
                )

            default_impl.__name__ = method_name
            return default_impl

        setattr(cls, name, make_default_impl(name))

    # No more abstract methods remain, so overwrite __abstactmethods__.
    cls.__abstractmethods__ = frozenset()

    return cls


# [START echo_server_impl]
class TestEchoer(ffx.EchoServer):
    def echo_string(
        self, request: ffx.EchoEchoStringRequest
    ) -> ffx.EchoEchoStringResponse:
        return ffx.EchoEchoStringResponse(response=request.value)
        # [END echo_server_impl]


class AsyncEchoer(ffx.EchoServer):
    async def echo_string(
        self, request: ffx.EchoEchoStringRequest
    ) -> ffx.EchoEchoStringResponse:
        await asyncio.sleep(0)  # This isn't necessary, but it is fun.
        return ffx.EchoEchoStringResponse(response=request.value)


class TargetCollectionReaderImpl(ffx.TargetCollectionReaderServer):
    def __init__(
        self, channel: Channel, target_list: list[ffx.TargetInfo]
    ) -> None:
        super().__init__(channel)
        self.target_list = target_list

    def next_(self, request: ffx.TargetCollectionReaderNextRequest) -> None:
        if not request.entry:
            raise StopServer
        self.target_list.extend(request.entry)


@implement_missing_abstract_methods
class TargetCollectionImpl(ffx.TargetCollectionServer):
    async def list_targets(
        self, request: ffx.TargetCollectionListTargetsRequest
    ) -> None:
        reader = ffx.TargetCollectionReaderClient(request.reader)
        await reader.next_(
            entry=[
                ffx.TargetInfo(nodename="foo"),
                ffx.TargetInfo(nodename="bar"),
            ]
        )
        await reader.next_(
            entry=[
                ffx.TargetInfo(nodename="baz"),
            ]
        )
        await reader.next_(entry=[])


@implement_missing_abstract_methods
class StubFileServer(f_io.FileServer):
    def read(
        self, request: f_io.ReadableReadRequest
    ) -> f_io.ReadableReadResponse:
        return f_io.ReadableReadResponse(data=[1, 2, 3, 4])


@implement_missing_abstract_methods
class NotImplementedCrossLibraryNoopServer(fc_othertest.CrossLibraryNoopServer):
    ...


@implement_missing_abstract_methods
class NotImplementedCrossLibraryNoopEventHandler(
    fc_othertest.CrossLibraryNoopEventHandler
):
    ...


class TestEventHandler(fc_othertest.CrossLibraryNoopEventHandler):
    def __init__(
        self,
        client: fc_othertest.CrossLibraryNoopClient,
        random_event_handler: typing.Callable[
            [fc_othertest.CrossLibraryNoopOnRandomEventRequest], None
        ],
    ):
        super().__init__(client)
        self.random_event_handler = random_event_handler

    def on_random_event(
        self, request: fc_othertest.CrossLibraryNoopOnRandomEventRequest
    ) -> None:
        self.random_event_handler(request)

    def on_empty_event(self) -> None:
        raise StopEventHandler


@implement_missing_abstract_methods
class FailingFileServer(f_io.FileServer):
    def read(self, _: f_io.ReadableReadRequest) -> DomainError:
        return DomainError(ZxStatus.ZX_ERR_PEER_CLOSED)


@implement_missing_abstract_methods
class TestingServer(fc_test.TestingServer):
    def return_union(self) -> fc_test.TestingReturnUnionResponse:
        return fc_test.TestingReturnUnionResponse(y="foobar")

    def return_union_with_table(
        self,
    ) -> fc_test.TestingReturnUnionWithTableResponse:
        return fc_test.TestingReturnUnionWithTableResponse(
            y=fc_test.NoopTable(str_="bazzz", integer=-2)
        )


class ServerTests(unittest.IsolatedAsyncioTestCase):
    async def test_echo_server_sync(self) -> None:
        # [START use_echoer_example]
        (tx, rx) = Channel.create()
        server = TestEchoer(rx)
        client = ffx.EchoClient(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")
        server_task.cancel()
        # [END use_echoer_example]

    async def test_epitaph_propagation(self) -> None:
        (tx, rx) = Channel.create()
        client = ffx.EchoClient(tx)
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
        with self.assertRaises(ZxStatus) as cm:
            await coro1
        self.assertEqual(cm.exception.args[0], err_msg)

        with self.assertRaises(ZxStatus) as cm:
            await task
        self.assertEqual(cm.exception.args[0], err_msg)

        with self.assertRaises(ZxStatus) as cm:
            await coro2
        self.assertEqual(cm.exception.args[0], err_msg)

        # Finally, ensure that the channel is just plain-old closed for new
        # interactions.
        with self.assertRaises(ZxStatus) as cm:
            await client.echo_string(value="foobar")
        self.assertEqual(cm.exception.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)

    async def test_echo_server_async(self) -> None:
        (tx, rx) = Channel.create()
        server = AsyncEchoer(rx)
        client = ffx.EchoClient(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")
        server_task.cancel()

    # TODO(https://fxbug.dev/364878315): This test sends many messages to AsyncEchoer which causes
    # many spurious wakeups as a side-effect. Having this test ensures ServerBase is resilient
    # to spurious wakeups, even several at a time.
    async def test_echo_server_async_stress(self) -> None:
        (tx, rx) = Channel.create()
        server = AsyncEchoer(rx)
        client = ffx.EchoClient(tx)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        for _ in range(12):
            res = await client.echo_string(value="foobar")
            self.assertEqual(res.response, "foobar")
        server_task.cancel()

    async def test_target_iterator(self) -> None:
        (reader_client_channel, reader_server_channel) = Channel.create()
        target_list: typing.List[typing.Any] = []
        server = TargetCollectionReaderImpl(reader_server_channel, target_list)
        (tc_client_channel, tc_server_channel) = Channel.create()
        target_collection_server = TargetCollectionImpl(tc_server_channel)  # type: ignore[abstract]
        loop = asyncio.get_running_loop()
        reader_task = loop.create_task(server.serve())
        tc_task = loop.create_task(target_collection_server.serve())
        tc_client = ffx.TargetCollectionClient(tc_client_channel)
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

    async def test_file_server(self) -> None:
        # This handles the kind of case where a method has a signature of `-> (data) error Error;`
        client, server = Channel.create()
        file_proxy = f_io.FileClient(client)
        print(StubFileServer.get_flags)
        file_server = StubFileServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(
            file_server.serve()
        )
        response = (await file_proxy.read(count=4)).unwrap()
        self.assertEqual(response.data, [1, 2, 3, 4])
        server_task.cancel()

    async def test_failing_file_server(self) -> None:
        client, server = Channel.create()
        file_proxy = f_io.FileClient(client)
        file_server = FailingFileServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(
            file_server.serve()
        )
        result = await file_proxy.read(count=4)
        self.assertEqual(result.err, ZxStatus.ZX_ERR_PEER_CLOSED)
        server_task.cancel()

    async def test_testing_server(self) -> None:
        client, server = Channel.create()
        t_client = fc_test.TestingClient(client)
        t_server = TestingServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        return_union_response = await t_client.return_union()
        assert return_union_response.y is not None
        self.assertEqual(return_union_response.y, "foobar")
        return_union_with_table_response = (
            await t_client.return_union_with_table()
        )
        assert return_union_with_table_response.y is not None
        self.assertEqual(return_union_with_table_response.y.str_, "bazzz")
        self.assertEqual(return_union_with_table_response.y.integer, -2)
        server_task.cancel()

    async def test_flexible_method_framework_err(self) -> None:
        @implement_missing_abstract_methods
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTesterServer):
            def some_method(self) -> FrameworkError:
                # This should be handled internally, but right now there's not really
                # a good way to force this interaction without making multiple FIDL
                # versions run in this program simultaneously somehow.
                return FrameworkError.UNKNOWN_METHOD

            def some_method_without_error(self) -> FrameworkError:
                return FrameworkError.UNKNOWN_METHOD

            def some_method_just_error(self) -> FrameworkError:
                return FrameworkError.UNKNOWN_METHOD

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTesterClient(client)
        t_server = FlexibleMethodTesterServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res1 = await t_client.some_method()
        self.assertEqual(res1.framework_err, FrameworkError.UNKNOWN_METHOD)
        res2 = await t_client.some_method_without_error()
        self.assertEqual(res2.framework_err, FrameworkError.UNKNOWN_METHOD)
        res3 = await t_client.some_method_just_error()
        self.assertEqual(res3.framework_err, FrameworkError.UNKNOWN_METHOD)
        server_task.cancel()

    async def test_flexible_method_unwrap_response(self) -> None:
        @implement_missing_abstract_methods
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTesterServer):
            def some_method(
                self,
            ) -> fc_test.FlexibleMethodTesterSomeMethodResponse:
                return fc_test.FlexibleMethodTesterSomeMethodResponse(
                    some_bool_value=True
                )

            def some_method_without_error(
                self,
            ) -> fc_test.FlexibleMethodTesterSomeMethodWithoutErrorResponse:
                return (
                    fc_test.FlexibleMethodTesterSomeMethodWithoutErrorResponse(
                        some_bool_value=False
                    )
                )

            def some_method_just_error(self) -> None:
                return

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTesterClient(client)
        t_server = FlexibleMethodTesterServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res1 = await t_client.some_method()
        self.assertTrue(res1.unwrap().some_bool_value)
        res2 = await t_client.some_method_without_error()
        self.assertFalse(res2.unwrap().some_bool_value)
        (await t_client.some_method_just_error()).unwrap()
        server_task.cancel()

    async def test_flexible_method_unwrap_error(self) -> None:
        @implement_missing_abstract_methods
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTesterServer):
            def some_method(self) -> DomainError:
                return DomainError(error=ZxStatus.ZX_ERR_INTERNAL)

            def some_method_without_error(self) -> FrameworkError:
                return FrameworkError.UNKNOWN_METHOD

            def some_method_just_error(self) -> DomainError:
                return DomainError(error=ZxStatus.ZX_ERR_INTERNAL)

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTesterClient(client)
        t_server = FlexibleMethodTesterServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        res1 = await t_client.some_method()
        with self.assertRaisesRegex(RuntimeError, "Result error"):
            res1.unwrap()
        res2 = await t_client.some_method_without_error()
        with self.assertRaisesRegex(RuntimeError, "Result framework error"):
            res2.unwrap()
        res3 = await t_client.some_method_just_error()
        with self.assertRaisesRegex(RuntimeError, "Result error"):
            res3.unwrap()
        server_task.cancel()

    async def test_strict_one_way_union(self) -> None:
        @implement_missing_abstract_methods
        class NoopServer(fc_test.NoopServer):
            def strict_one_way_union(self, value: Any) -> None:
                pass

        client, server = Channel.create()
        t_client = fc_test.NoopClient(client)
        t_server = NoopServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        t_client.strict_one_way_union(value=1)
        server_task.cancel()

    async def test_strict_two_way_union(self) -> None:
        @implement_missing_abstract_methods
        class NoopServer(fc_test.NoopServer):
            def strict_two_way_union(self, value: Any) -> None:
                return

        client, server = Channel.create()
        t_client = fc_test.NoopClient(client)
        t_server = NoopServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        await t_client.strict_two_way_union(other_value="foo")
        server_task.cancel()

    async def test_flexible_one_way_union(self) -> None:
        @implement_missing_abstract_methods
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTesterServer):
            def flexible_one_way_union(
                self,
                value: fc_test.FlexibleMethodTesterFlexibleOneWayUnionRequest,
            ) -> None:
                pass

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTesterClient(client)
        t_server = FlexibleMethodTesterServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        t_client.flexible_one_way_union(value=1)
        server_task.cancel()

    async def test_flexible_two_way_union(self) -> None:
        @implement_missing_abstract_methods
        class FlexibleMethodTesterServer(fc_test.FlexibleMethodTesterServer):
            def flexible_two_way_union(self, value: Any) -> None:
                return

        client, server = Channel.create()
        t_client = fc_test.FlexibleMethodTesterClient(client)
        t_server = FlexibleMethodTesterServer(server)  # type: ignore[abstract]
        server_task = asyncio.get_running_loop().create_task(t_server.serve())
        (await t_client.flexible_two_way_union(other_value="foo")).unwrap()
        server_task.cancel()

    async def test_sending_and_receiving_event(self) -> None:
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoopClient(client)
        THIS_EXPECTED = 3
        THAT_EXPECTED = fc_othertest.TestingEnum.FLIPPED_OTHER_TEST

        def random_event_handler(
            request: fc_othertest.CrossLibraryNoopOnRandomEventRequest,
        ) -> None:
            self.assertEqual(request.this, THIS_EXPECTED)
            self.assertEqual(request.that, THAT_EXPECTED)
            raise StopEventHandler

        # It's okay to use an unimplemented server here since we're not fielding any calls.
        t_server = NotImplementedCrossLibraryNoopServer(server)  # type: ignore[abstract]
        event_handler = TestEventHandler(t_client, random_event_handler)
        t_server.on_random_event(this=THIS_EXPECTED, that=THAT_EXPECTED)
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        # If the event is never received this will loop forever (since the channel is open, and the
        # handler should stop service after receiving the event.
        await task

    async def test_sending_and_receiving_empty_event(self) -> None:
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoopClient(client)
        # It's okay to use an unimplemented server here since we're not fielding any calls.
        t_server = NotImplementedCrossLibraryNoopServer(server)  # type: ignore[abstract]
        event_handler = TestEventHandler(t_client, lambda _: None)
        t_server.on_empty_event()
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        # If the event is never received this will loop forever (since the channel is open, and the
        # handler should stop service after receiving the event.
        await task

    async def test_closing_channel_closes_event_loop(self) -> None:
        client, server = Channel.create()
        t_client = fc_othertest.CrossLibraryNoopClient(client)
        del server
        # A generic unimplemented event handler is fine, since we're just making it exit.
        event_handler = NotImplementedCrossLibraryNoopEventHandler(t_client)  # type: ignore[abstract]
        task = asyncio.get_running_loop().create_task(event_handler.serve())
        await task
