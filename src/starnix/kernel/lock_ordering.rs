// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lock_sequence::impl_lock_after;
pub(crate) use lock_sequence::Unlocked;

use crate::{bpf::lock_levels::BpfMapEntries, task::kernel_lock_levels::KernelIpTables};

// This file defines a hierarchy of locks, that is, the order in which
// the locks must be acquired. Unlocked is a highest level and represents
// a state in which no locks are held.

impl_lock_after!(Unlocked => BpfMapEntries);
impl_lock_after!(Unlocked => KernelIpTables);
