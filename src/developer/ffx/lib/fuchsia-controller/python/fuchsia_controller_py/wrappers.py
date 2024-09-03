# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
from typing import Any, Callable, Coroutine, ParamSpec, TypeVar

_Params = ParamSpec("_Params")
_Yield = TypeVar("_Yield")
_Send = TypeVar("_Send")
_Ret = TypeVar("_Ret")


class AsyncAdapterError(Exception):
    """Raised when an asyncmethod is used outside of an AsyncAdapter."""


class AsyncAdapter:
    """A wrapper or mixin that supports async calls in a synchronous context.

    This can be used with any object where you wish to expose functions as
    synchronous when in reality they are implemented in async. This is for
    convenience in areas like Mobly where tests are expected to be run as
    synchronous methods. Or in places where you intend to have things like
    `asyncio.Queue` used across multiple function calls.

    The implementation is simple: the class using this adapter is given an
    async loop to itself. This is the main loop used for every function call
    to this object.

    To expose an async function as synchronous, just use the `asyncmethod`
    decorator.

    For example:

    ```python
    class TestClass(AsyncAdapter, BaseTestClass):

        @asyncmethod
        async def foo(self) -> None:
            await asyncio.sleep(1)
    ```

    In the above, the `foo` method will be exposed as a synchronous method,
    but inside it is async code.

    If you're using this AsyncAdapter as a mixin and you're getting exceptions
    when using the `asyncmethod` decorator, make sure to put this first in the
    inheritance order to ensure proper initialization based on Python's
    method resolution order.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._async_adapter_loop = asyncio.new_event_loop()

    def loop(self) -> asyncio.AbstractEventLoop:
        """Returns a copy of this class's event loop.

        This is intended for spawning tasks in this class.
        """
        return self._async_adapter_loop


def asyncmethod(
    func: Callable[_Params, Coroutine[_Yield, _Send, _Ret]],
) -> Callable[_Params, _Ret]:
    """A decorator to expose an async method as synchronous.

    This should ONLY be used with classes that inherit `AsyncAdapter`.
    """

    def wrapper(*args: _Params.args, **kwargs: _Params.kwargs) -> _Ret:
        coro = func(*args, **kwargs)
        try:
            loop = getattr(args[0], "_async_adapter_loop")  # args[0] == self
            return loop.run_until_complete(coro)
        except AttributeError as e:
            raise AsyncAdapterError(
                "`asyncmethod` was used outside of an `AsyncAdapter`. "
                + "Your class must inherit from "
                + "`fuchsia_controller_py.wrappers.AsyncAdapter` to use this "
                + "decorator. If you're already inheriting this and you're "
                + "seeing this exception, put `AsyncAdapter` first in your "
                + "inheritance order."
            ) from e

    return wrapper
