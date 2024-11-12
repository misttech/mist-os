// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/userloader/elf.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/types.h>

#include <zxtest/zxtest.h>

namespace {

const zx_vaddr_t kTestStackAddr = 0x30000000;

TEST(StackTest, TrivialInitialStack) {
  zx::vmo stack_vmo;
  ASSERT_OK(zx::vmo::create(0x4000, 0u, &stack_vmo));

  zx_vaddr_t original_stack_start_addr = kTestStackAddr + 0x1000;

  fbl::String path("");
  fbl::Vector<fbl::String> argv;
  fbl::Vector<fbl::String> environ;
  fbl::Vector<ktl::pair<uint32_t, uint64_t>> auxv;

  auto stack_start_addr = populate_initial_stack(zx::debuglog(), stack_vmo, path, argv, environ,
                                                 auxv, kTestStackAddr, original_stack_start_addr);

  size_t argc_size = 8;
  size_t argv_terminator_size = 8;
  size_t environ_terminator_size = 8;
  size_t aux_execfn_terminator_size = 8;
  size_t aux_execfn = 16;
  size_t aux_random = 16;
  size_t aux_null = 16;
  size_t random_seed = 16;

  size_t payload_size = argc_size + argv_terminator_size + environ_terminator_size +
                        aux_execfn_terminator_size + aux_execfn + aux_random + aux_null +
                        random_seed;
  payload_size += payload_size % 16;
  ASSERT_EQ(stack_start_addr.value().stack_pointer, original_stack_start_addr - payload_size);
}

}  // namespace
