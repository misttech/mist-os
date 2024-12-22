# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Defines the import hooks for when a user writes `import fidl.[fidl_library]`."""

# autoflake: skip_file
import importlib.util
import sys
from collections.abc import Sequence
from importlib.abc import MetaPathFinder
from importlib.machinery import ModuleSpec
from types import ModuleType

from ._async_socket import AsyncSocket
from ._fidl_common import EpitaphError, FrameworkError
from ._ipc import GlobalHandleWaker, HandleWaker
from ._library import FIDLLibraryModule, load_fidl_module


class FIDLImportFinder(MetaPathFinder):
    """The main import hook class."""

    def find_spec(
        self,
        fullname: str,
        path: Sequence[str] | None = None,
        target: ModuleType | None = None,
    ) -> ModuleSpec | None:
        # TODO(https://fxbug.dev/42061151): Remove "TransportError".
        if (
            fullname.startswith("fidl._")
            or fullname == "fidl.FrameworkError"
            or fullname == "fidl.TransportError"
        ):
            return None
        if fullname.startswith("fidl."):
            # TODO(https://fxbug.dev/346628306): Need to determine how to make mypy
            # correctly check the types for a Loader.
            return importlib.util.spec_from_loader(fullname, FIDLImportLoader)  # type: ignore
        return None


class FIDLImportLoader:
    # TODO(https://fxbug.dev/346628306): Need to determine how to make mypy
    # correctly check the types for a Loader.
    def create_module(spec: ModuleSpec) -> ModuleType:  # type: ignore
        return load_fidl_module(spec.name)

    # TODO(https://fxbug.dev/346628306): Need to determine how to make mypy
    # correctly check the types for a Loader.
    def exec_module(module: ModuleType) -> None:  # type: ignore
        pass


meta_hook = FIDLImportFinder()
sys.meta_path.insert(0, meta_hook)
