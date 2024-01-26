// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The hooks modules implement SELinux hooks to control and enforce access decisions, and define
// SELinux security structs associated to objects in the system, holding state information used
// by hooks to make access decisions.

pub mod current_task_hooks;
pub mod thread_group_hooks;
