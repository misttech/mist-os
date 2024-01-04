# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Defines the import hooks for when a user writes `import fidl.[fidl_library]`."""
import importlib.abc
import sys

from ._async_socket import AsyncSocket
from ._fidl_common import FrameworkError
from ._fidl_common import EpitaphError
from ._ipc import GlobalHandleWaker
from ._ipc import HandleWaker
from ._library import load_module


class FIDLImportFinder(importlib.abc.MetaPathFinder):
    """The main import hook class."""

    def find_module(self, fullname: str, path=None):
        """Override from abc.MetaPathFinder."""
        # TODO(fxbug.dev/109789): Remove "TransportError".
        if (
            fullname.startswith("fidl._")
            or fullname == "fidl.FrameworkError"
            or fullname == "fidl.TransportError"
        ):
            return __loader__
        elif fullname.startswith("fidl."):
            return self

    def load_module(self, fullname: str):
        """Override from abc.MetaPathFinder."""
        return load_module(fullname)


meta_hook = FIDLImportFinder()
sys.meta_path.insert(0, meta_hook)
