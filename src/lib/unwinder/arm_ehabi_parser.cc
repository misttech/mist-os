// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/arm_ehabi_parser.h"

#include <elf.h>

#include "src/lib/unwinder/arm_ehabi_module.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {
namespace {
constexpr uint32_t kExIdxCantUnwind = 1;
constexpr uint32_t kFinishedIndicator = 0xb0;

uint8_t GetNextByteFromData(uint32_t data, size_t offset) {
  const uint8_t* byte_data = reinterpret_cast<uint8_t*>(&data);

  // This reverses the byte order from a little endian representation to be more like big endian
  // to make decoding simpler. The encoding scheme typically will have us walk from most significant
  // to least significant bytes within a 16 or 32 bit range and interpret different bit pairings
  // within there so we need to be careful to take things in the correct order. This works for
  // arbitrary offsets broken on a 4 byte boundary. For example, for offsets 0, 1, 2, 3, 4, ... the
  // returned bytes will be at offsets 3, 2, 1, 0, 7, ... and so on. Since this unwinder is
  // out-of-process, this function won't work for data beyond the particular 32 bits in |data|, the
  // rest of the data must be fetched from the remote process.
  //
  // An example: if you have an encoding for popping a register off the stack (see EHABI 10.3)
  // the binary encoding looks like this:
  //
  //   0b1000iiii iiiiiiii
  //         ^    ^
  //         |    |
  //         r15
  //          r14
  //           r13
  //            r12
  //              |
  //              r11
  //             ..
  //                    r5
  //                     r4
  //
  // On a little endian system this will look like this in memory:
  //
  //   0biiiiiiii iiii0010
  //     ^        ^
  //     |        |
  //     |        r13
  //     |         r12
  //     |          r15
  //     |           r14
  //     r5
  //      r4
  //       r7
  //        r6
  //
  // So when you ask for offset 0 from this 16 bit integer, you'll get back
  //
  //   0b1000iiii
  //         ^
  //         |
  //         r15
  //          r14
  //           r13
  //            r12
  //
  // And offset 1 will be
  //
  //   0biiiiiiii
  //     ^
  //     |
  //     r11
  //      r10
  //        ..
  //           r5
  //            r4
  return byte_data[(offset & ~static_cast<size_t>(0x03)) +
                   (3 - (offset & static_cast<size_t>(0x03)))];
}

}  // namespace

ArmEhAbiParser::ArmEhAbiParser(const ArmEhAbiModule::IdxHeader& entry) {
  // TODO(https://fxbug.dev/430572991): Support decoding from the .ARM.extab section as well.
  switch (entry.type) {
    case ArmEhAbiModule::IdxHeader::Type::kCompact:
      extab_offset_ = static_cast<int32_t>(entry.header.data);
      break;
    case ArmEhAbiModule::IdxHeader::Type::kCompactInline:
      data_ = entry.header.data;
      break;
    default:
      break;
  }
}

Error ArmEhAbiParser::Step(Memory* stack, const Registers& current, Registers& next) {
  if (data_ == 0 && extab_offset_ == 0) {
    return Error("Invalid IdxHeaderEntry.");
  } else if (data_ == kExIdxCantUnwind || extab_offset_ == kExIdxCantUnwind) {
    return Error("Got ExIdxCantUnwind.");
  }

  // The ARM EH ABI specifies that we're working with a singular "virtual register set" which is
  // always updated in place. We don't have that concept here, so every step operation has to start
  // with updating all of the registers in |next| to |current| with the exception of PC, which will
  // be set explicitly by one of the unwinding instructions or to LR (r14) if not set explicitly by
  // the end of the instructions.
  for (size_t i = 0; i < static_cast<size_t>(RegisterID::kArm32_last); i++) {
    uint64_t v;
    if (i != static_cast<size_t>(RegisterID::kArm32_pc)) {
      // Not an error if something isn't set here.
      if (current.Get(static_cast<RegisterID>(i), v).ok()) {
        next.Set(static_cast<RegisterID>(i), v);
      }
    }
  }

  Error err = Success();
  if (data_ > 0) {
    err = ParseCompactFromWord(stack, data_, next);
  } else if (extab_offset_ > 0) {
    err = Error("Offset not handled yet.");
  }

  if (err.has_err()) {
    return err;
  }

  // Finally, restore PC to LR if not directly set by the unwinding instructions.
  if (uint64_t pc; next.GetPC(pc).has_err()) {
    // Undefined LR register usually means the end of unwinding. This is not considered an error.
    if (uint64_t return_address; next.GetReturnAddress(return_address).ok()) {
      err = next.SetPC(return_address);
    } else {
      err = Error("Could not set PC from LR!");
    }
  }

  return err;
}

Error ArmEhAbiParser::ParseCompactFromWord(Memory* stack, uint32_t data, Registers& next) {
  // Start from offset 1, because we know that the first byte is the encoding for inline data or
  // not.
  size_t offset = 1;

  FrameHandlerType type = static_cast<FrameHandlerType>((data & 0x0f000000) >> 24);

  // The number of 32 bit words _after_ |data| that we need to read for all of the unwinding
  // instructions.
  size_t num_extra_words;
  switch (type) {
    case FrameHandlerType::kSu16:
      num_extra_words = 0;  // All instructions present in |data|.
      break;
    case FrameHandlerType::kLu16:
    case FrameHandlerType::kLu32:
      num_extra_words = (data & 0x00ff0000) >> 16;
      break;
    default:
      return Error("Unknown FrameHandlerType: %d\n", static_cast<uint16_t>(type));
  }

  if (num_extra_words == 0) {
    // Parse instructions starting at byte offset 1.
    return ParseInstructionsFromOffset(stack, data, offset, type, next);
  }

  return Error("num_extra_words > 0 not handled yet");
}

Error ArmEhAbiParser::ParseInstructionsFromOffset(Memory* stack, uint32_t data, size_t offset,
                                                  FrameHandlerType type, Registers& next) {
  uint8_t byte = GetNextByteFromData(data, offset++);

  while (byte != kFinishedIndicator) {
    // Modifying SP with an immediate.
    if ((byte & 0x80) == 0) {
      uint64_t sp;
      if (auto err = next.GetSP(sp); err.has_err()) {
        return err;
      }

      // The addend is in the lower 6 bits of this byte.
      if (byte & 0x40) {
        // sp = sp - (XXXXXX << 2) - 4
        //    = sp - ((XXXXXX << 2) + 4)
        // Make sure to mask away the high bits so we don't accidentally subtract a negative.
        sp -= ((static_cast<uint32_t>(byte) & 0x3f) << 2) + 4;
      } else {
        // sp = sp + (XXXXXXX << 2) + 4
        sp += (static_cast<uint32_t>(byte) << 2) + 4;
      }

      if (auto err = next.SetSP(sp); err.has_err()) {
        return err;
      }
    } else {
      // Have to decode the rest of the bits in the high nibble now that we know that MSB == 1.
      switch (byte & 0xf0) {
        case 0x80: {
          // Pop register values from stack.
          // Registers r4-r15 are packed into the low 4 bits of this byte and the entire next byte.
          // This mask decodes those low 12 bits and creates a mask where each register is in it's
          // respective bit position in the mask (i.e. r15 is in bit 15, r14 in bit 14, etc).
          uint32_t register_mask =
              (((static_cast<uint32_t>(byte & 0x0f)) << 12) |
               (static_cast<uint32_t>(GetNextByteFromData(data, offset++))) << 4);

          if (auto err = SetRegistersFromMask(stack, register_mask, next); err.has_err()) {
            return err;
          }
          break;
        }
        case 0x90: {
          // Set SP from a register.
          uint32_t reg = byte & 0x0f;
          if (reg == 13 || reg == 15) {
            return Error("Setting SP from register 13 or 15 is disallowed.");
          }

          uint64_t val;
          if (auto err = next.Get(static_cast<RegisterID>(reg), val); err.has_err()) {
            return err;
          }

          if (auto err = next.SetSP(val); err.has_err()) {
            return err;
          }
          break;
        }
        case 0xa0: {
          // Pop registers from r4 thru r[4 + N] (inclusive).
          // N is encoded in the lower 3 bits. We'll create a register mask same as above, where
          // each bit corresponds the register index.
          uint32_t n = byte & 0x3;

          // Add 1 to |n| to get 1 past the last register we want to include. After shifting,
          // subtract 1 to set all the lower bits to 1 and shift up by 4 so the mask ends with r4.
          uint32_t register_mask = ((1u << (n + 1)) - 1) << 4;

          // If the high bit of the low nibble is set, we also pop r14.
          if (byte & 0x08) {
            register_mask |= 1 << 14;
          }

          if (auto err = SetRegistersFromMask(stack, register_mask, next); err.has_err()) {
            return err;
          }

          break;
        }
        default: {
          return Error("OP %#x not implemented yet", byte);
        }
      }
    }

    if (offset > 3) {
      break;
    }

    byte = GetNextByteFromData(data, offset++);
  }

  return Success();
}

Error ArmEhAbiParser::SetRegistersFromMask(Memory* stack, uint32_t register_mask, Registers& next) {
  uint64_t sp;
  if (auto err = next.GetSP(sp); err.has_err()) {
    return err;
  }

  bool set_sp = false;
  for (size_t i = 0; i < static_cast<size_t>(RegisterID::kArm32_last); i++) {
    if (register_mask & (1 << i)) {
      uint32_t val;
      if (auto err = stack->ReadAndAdvance(sp, val); err.has_err()) {
        return err;
      }

      if (static_cast<RegisterID>(i) == RegisterID::kArm32_sp) {
        set_sp = true;
      }

      next.Set(static_cast<RegisterID>(i), val);
    }
  }

  if (!set_sp) {
    // SP wasn't popped by the bitmask above, so we set it to the value after popping all registers
    // from the mask.
    next.SetSP(sp);
  }

  return Success();
}

}  // namespace unwinder
