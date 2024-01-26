// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{impl_lock_after, lock_level, Unlocked};

lock_level!(BpfMapEntries);
lock_level!(KernelIpTables);
lock_level!(KernelSwapFiles);
lock_level!(DiagnosticsCoreDumpList);

lock_level!(MmDumpable);

// Artificial lock level that is used when releasing a Task
lock_level!(TaskRelease);
lock_level!(ProcessGroupState);

// FileOps lock levels. These are artificial lock levels used to call methods of FileOps traits.
lock_level!(FileOpsRead);
lock_level!(FileOpsWrite);
lock_level!(FileOpsIoctl);

// This file defines a hierarchy of locks, that is, the order in which
// the locks must be acquired. Unlocked is a highest level and represents
// a state in which no locks are held.

impl_lock_after!(Unlocked => BpfMapEntries);
impl_lock_after!(Unlocked => KernelIpTables);
impl_lock_after!(Unlocked => KernelSwapFiles);
impl_lock_after!(Unlocked => DiagnosticsCoreDumpList);
impl_lock_after!(Unlocked => MmDumpable);
impl_lock_after!(Unlocked => TaskRelease);

impl_lock_after!(TaskRelease => FileOpsIoctl);
// FileOpsIoctl is before read/write because SocketFile.ioctl ends up indirectly calling FileOps.read and FileOps.write
impl_lock_after!(FileOpsIoctl => FileOpsRead);
impl_lock_after!(FileOpsRead => FileOpsWrite);
impl_lock_after!(FileOpsWrite => ProcessGroupState);
