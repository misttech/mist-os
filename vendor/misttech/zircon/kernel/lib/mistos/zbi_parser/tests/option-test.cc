// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/userabi/userboot.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/zbi.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>

#include <zxtest/zxtest.h>

namespace {

TEST(Option, Parse) {
  const zx::unowned_vmo zbi{userboot::gVmos[userboot::kZbi]};
  zx::vmar vmar_self{VmAspace::kernel_aspace()->RootVmar()};

  auto get_opts = zbi_parser::GetOptionsFromZbi(vmar_self, *zbi);
  ASSERT_FALSE(get_opts.is_error());

  // zbi_parser::Options opts{get_opts.value()};
}

}  // namespace
