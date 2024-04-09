# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Python Types for Assembly Configuration Files

This module contains Python classes for working with files that have the same
schema as `//src/developer/ffx/plugins/assembly`.
"""

from dataclasses import dataclass, field
from typing import Optional, Set, TypeVar

import serialization

__all__ = ["ImageAssemblyConfig", "KernelInfo"]

from .common import FileEntry, FilePath

ExtendsImageAssemblyConfig = TypeVar(
    "ExtendsImageAssemblyConfig", bound="ImageAssemblyConfig"
)


@dataclass
class KernelInfo:
    """Information about the kernel"""

    path: Optional[FilePath] = None
    args: Set[str] = field(default_factory=set)


@dataclass
@serialization.serialize_json
class ImageAssemblyConfig:
    """The input configuration for the Image Assembly Operation

    This describes all the packages, bootfs files, kernel args, kernel, etc.
    that are to be combined into a complete set of assembled product images.
    """

    base: Set[FilePath] = field(default_factory=set)
    cache: Set[FilePath] = field(default_factory=set)
    system: Set[FilePath] = field(default_factory=set)
    kernel: KernelInfo = field(default_factory=KernelInfo)
    qemu_kernel: Optional[FilePath] = None
    boot_args: Set[str] = field(default_factory=set)
    bootfs_files: Set[FileEntry] = field(default_factory=set)
    bootfs_packages: Set[FilePath] = field(default_factory=set)
    devicetree: Optional[FilePath] = None

    def __repr__(self) -> str:
        """Serialize to a JSON string"""
        return serialization.json_dumps(self, indent=2)
