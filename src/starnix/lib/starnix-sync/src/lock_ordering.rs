// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Unlocked;
use lock_ordering_macro::lock_ordering;

lock_ordering! {
    // Kernel.iptables
    Unlocked => KernelIpTables,
    // Kernel.swap_files
    Unlocked => KernelSwapFiles,
    // MemoryManager.dumpable
    Unlocked => MmDumpable,
    // Artificial lock level for {Task, CurrentTask}.release()
    Unlocked => TaskRelease,
    // ProcessGroup.mutable_state.
    // Needs to be before TaskRelease because of the dependency in CurrentTask.release()
    TaskRelease => ProcessGroupState,
    // Artificial level for ResourceAccessor.add_file_with_flags(..)
    Unlocked => ResourceAccessorAddFile,
    // Artificial level for FileOps.to_handle(..)
    ResourceAccessorAddFile => FileOpsToHandle,
    // Artificial level for DeviceOps.open(..)
    FileOpsToHandle => DeviceOpen,
    // Artificial level for several FileOps and FsNodeOps method forming a connected group
    // because of dependencies between them: FileOps.read, FsNode.create_file_ops, ...
    DeviceOpen => FileOpsCore,
    // ProcessGroup.mutable_state. Artificial locks above need to be before it because of
    // dependencies in DevPtsFile.{read, write, ioctl}.
    FileOpsCore => ProcessGroupState,
    // Bpf locks
    FileOpsCore => BpfHelperOps,
    BpfHelperOps => BpfMapEntries,
    // Artificial level for methods in FsNodeOps/FileOps that require access to the
    // FsNode.append_lock
    Unlocked => BeforeFsNodeAppend,
    // FsNode.append_lock
    BeforeFsNodeAppend => FsNodeAppend,
    FsNodeAppend => FileOpsCore,
}
