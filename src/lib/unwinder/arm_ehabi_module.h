// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_ARM_EHABI_MODULE_H_
#define SRC_LIB_UNWINDER_ARM_EHABI_MODULE_H_

#include "gtest/gtest_prod.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

inline uint32_t SignExtendPrel31(uint32_t data) { return data | ((data & 0x40000000u) << 1); }

inline int32_t DecodePrel31(uint32_t ptr) { return static_cast<int32_t>(SignExtendPrel31(ptr)); }

class ArmEhAbiModule {
 public:
  ArmEhAbiModule(Memory* elf, uint32_t elf_ptr) : elf_(elf), elf_ptr_(elf_ptr) {}

  // Load the .ARM.exidx binary search table.
  [[nodiscard]] Error Load();

  [[nodiscard]] Error Step(Memory* stack, const Registers& current, Registers& next);

  struct IdxHeaderData {
    uint32_t fn_addr = 0;
    // Either the encoded handling table entry if the high bit is 1, otherwise a prel31 encoded
    // offset from the start of the table to the handling table entry in ARM.extab.
    uint32_t data = 0;
  };

  struct IdxHeader {
    IdxHeaderData header;

    enum class Type {
      // |header.data| is an offset into .ARM.extab for the unwinding instructions.
      kCompact,
      // |header.data| is an inlined compact model containing the unwinding instructions directly.
      kCompactInline,
      // The encoding instructions are inlined into |header.data|.
      kUnknown,
    } type = Type::kUnknown;
  };

 private:
  FRIEND_TEST(ArmEhAbiModule, Search);

  // Performs an upper bounds search for PC in the exidx table.
  Error Search(uint32_t pc, IdxHeader& entry);

  Memory* const elf_ = nullptr;
  const uint32_t elf_ptr_ = 0;

  // This is the start of the binary search lookup table. Each table entry is two 32 bit integers.
  uint32_t arm_exidx_start_ = 0;
  uint32_t arm_exidx_end_ = 0;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_ARM_EHABI_MODULE_H_
