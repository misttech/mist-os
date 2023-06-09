# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Defines the import hooks for when a user writes `import fidl.[fidl_library]`."""
import importlib.abc
import sys
import types

from ._library import get_fidl_ir_map
from ._library import load_module


class FIDLImportFinder(importlib.abc.MetaPathFinder):
    """The main import hook class."""

    def find_module(self, fullname: str, path=None):
        """Override from abc.MetaPathFinder."""
        if fullname.startswith("fidl.") and not fullname.startswith("fidl._"):
            return self
        else:
            return None

    def load_module(self, fullname: str):
        """Override from abc.MetaPathFinder."""
        return load_module(fullname)


meta_hook = FIDLImportFinder()
sys.meta_path.insert(0, meta_hook)
