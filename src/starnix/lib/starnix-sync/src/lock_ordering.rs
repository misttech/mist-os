// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Unlocked;
use lock_ordering_macro::lock_ordering;

lock_ordering! {
    // UninterruptibleLock represents a virtual level before which lock must be interruptible.
    Unlocked => UninterruptibleLock,
    // Artificial level for ResourceAccessor.add_file_with_flags(..)
    Unlocked => ResourceAccessorAddFile,
    // Artificial level for DeviceOps.open(..)
    ResourceAccessorAddFile => DeviceOpen,
    // Artificial level for several FileOps and FsNodeOps method forming a connected group
    // because of dependencies between them: FileOps.read, FsNode.create_file_ops, ...
    DeviceOpen => FileOpsCore,
    // Artificial level for methods in FsNodeOps/FileOps that require access to the
    // FsNode.append_lock
    Unlocked => BeforeFsNodeAppend,
    // FsNode.append_lock
    BeforeFsNodeAppend => FsNodeAppend,
    FsNodeAppend => FileOpsCore,
    // FileOpsCore are interruptible
    FileOpsCore => UninterruptibleLock,
    // Artificial lock level for {Task, CurrentTask}.release()
    Unlocked => TaskRelease,
    // During task release, file must be closed.
    TaskRelease => FileOpsCore,
    // Kernel.iptables
    UninterruptibleLock => KernelIpTables,
    // Kernel.swap_files
    UninterruptibleLock => KernelSwapFiles,
    // MemoryManager.dumpable
    UninterruptibleLock => MmDumpable,
    // ProcessGroup.mutable_state.
    // Needs to be before TaskRelease because of the dependency in CurrentTask.release()
    TaskRelease => ProcessGroupState,
    UninterruptibleLock => ProcessGroupState,
    // ProcessGroup.mutable_state. Artificial locks above need to be before it because of
    // dependencies in DevPtsFile.{read, write, ioctl}.
    FileOpsCore => ProcessGroupState,
    // Bpf locks
    FileOpsCore => BpfHelperOps,
    BpfHelperOps => BpfMapEntries,
    UninterruptibleLock => BpfMapEntries,
}
