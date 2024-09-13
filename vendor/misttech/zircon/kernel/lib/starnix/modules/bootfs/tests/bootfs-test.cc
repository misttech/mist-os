// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/modules/bootfs/bootfs.h>
#include <lib/unittest/unittest.h>

#include <object/handle.h>

#include "data/bootfs.zbi.h"
#include "zbi_file.h"

namespace unit_testing {

bool test_bootfs() {
  BEGIN_TEST;

  ZbiFile zbi;
  zbi.Write({kBootFsZbi, sizeof(kBootFsZbi) - 1});

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto fs = bootfs::BootFs::new_fs(kernel, HandleOwner(ktl::move(zbi).Finish()));

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_bootfs)
UNITTEST("test bootfs", unit_testing::test_bootfs)
UNITTEST_END_TESTCASE(starnix_fs_bootfs, "starnix_fs_bootfs", "Tests for starnix bootfs")
