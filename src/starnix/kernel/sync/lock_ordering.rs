// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{impl_lock_after, lock_level, Unlocked};

lock_level!(BpfHelperOps);
lock_level!(BpfMapEntries);
lock_level!(KernelIpTables);
lock_level!(KernelSwapFiles);
lock_level!(DiagnosticsCoreDumpList);

lock_level!(MmDumpable);

// Artificial lock level that is used when releasing a Task
lock_level!(TaskRelease);
lock_level!(ProcessGroupState);

// These lock levels are use to denote operations on FileOps, SocketOps and FsNodeOps
// TODO(https://fxbug.dev/324065824): FsNode.create_file_ops, FsNode.mknod, FsNode.unlink, FileOps.read,
// SocketOps.read use the same level because of the circular dependencies between them.
lock_level!(FileOpsCore);
lock_level!(WriteOps);

// Lock level for DeviceOps.open. Must be before FileOpsCore because of get_or_create_loop_device
lock_level!(DeviceOpen);

// This file defines a hierarchy of locks, that is, the order in which
// the locks must be acquired. Unlocked is a highest level and represents
// a state in which no locks are held.

impl_lock_after!(Unlocked => KernelIpTables);
impl_lock_after!(Unlocked => KernelSwapFiles);
impl_lock_after!(Unlocked => DiagnosticsCoreDumpList);
impl_lock_after!(Unlocked => MmDumpable);

impl_lock_after!(Unlocked => TaskRelease);
impl_lock_after!(TaskRelease => DeviceOpen);
impl_lock_after!(DeviceOpen => FileOpsCore);
impl_lock_after!(FileOpsCore => WriteOps);
impl_lock_after!(WriteOps =>  ProcessGroupState);
impl_lock_after!(WriteOps => BpfHelperOps);
impl_lock_after!(BpfHelperOps => BpfMapEntries);
