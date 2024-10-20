// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/fs/mistos/syslog.h"

#include <lib/mistos/starnix/kernel/task/module.h>
#include <lib/mistos/starnix/kernel/vfs/anon_node.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

namespace starnix {

FileHandle SyslogFile::new_file(const CurrentTask& current_task) {
  fbl::AllocChecker ac;
  auto file = ktl::make_unique<SyslogFile>(&ac);
  ASSERT(ac.check());
  return Anon::new_file(current_task, std::move(file), OpenFlags(OpenFlagsEnum::RDWR));
}

}  // namespace starnix
