// Copyright 2024 Mist Tecnologia. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/module.h"

#include <lib/mistos/starnix/kernel/task/module.h>

namespace starnix {

void DelayedReleaser::flush_file(FileHandle file, FdTableId id) const {}

}  // namespace starnix