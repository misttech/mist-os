// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_ARM_EHABI_PARSER_H_
#define SRC_LIB_UNWINDER_ARM_EHABI_PARSER_H_

#include "src/lib/unwinder/arm_ehabi_module.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

class ArmEhAbiParser {
 public:
  explicit ArmEhAbiParser(const ArmEhAbiModule::IdxHeader& entry);

  [[nodiscard]] Error Step(Memory* stack, const Registers& current, Registers& next);

 private:
  enum class FrameHandlerType : uint16_t {
    // All instructions are within a single 32 bit word.
    kSu16 = 0x00,
    // The next 16 bits are a number of 16 bit instructions to parse from the extab.
    kLu16 = 0x01,
    // The next 16 bits are a number of 32 bit instructions to parse from the extab.
    kLu32 = 0x02,
  };

  // Given a mask of registers where the bit index corresponds to the register number, pop from the
  // stack (from low register -> high register), and store them in |next|.
  Error SetRegistersFromMask(Memory* stack, uint32_t register_mask, Registers& next);

  Error ParseCompactFromWord(Memory* stack, uint32_t data, Registers& next);
  Error ParseInstructionsFromOffset(Memory* stack, uint32_t data, size_t offset,
                                    FrameHandlerType type, Registers& next);

  int32_t extab_offset_ = 0;
  uint32_t data_ = 0;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_ARM_EHABI_PARSER_H_
