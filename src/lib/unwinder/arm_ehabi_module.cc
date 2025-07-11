// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/arm_ehabi_module.h"

#include <elf.h>

#include "src/lib/unwinder/arm_ehabi_parser.h"
#include "src/lib/unwinder/elf_utils.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

Error ArmEhAbiModule::Load() {
  Elf32_Ehdr ehdr;
  if (auto err = elf_->Read(elf_ptr_, ehdr); err.has_err()) {
    return err;
  }

  if (!elf_utils::VerifyElfIdentification<Elf32_Ehdr>(ehdr, elf_utils::ElfClass::k32Bit)) {
    return Error("This doesn't look like an ELF module.");
  }

  Elf32_Phdr phdr;
  if (auto err = elf_utils::GetSegmentByType(elf_, elf_ptr_, PT_ARM_EXIDX, ehdr, phdr);
      err.has_err()) {
    return err;
  }

  arm_exidx_start_ = elf_ptr_ + phdr.p_vaddr;
  arm_exidx_end_ = arm_exidx_start_ + phdr.p_memsz;

  // TODO(https://fxbug.dev/430572991): Support fetching the .ARM.extab section here as well.

  return Success();
}

Error ArmEhAbiModule::Search(uint32_t pc, IdxHeader& entry) {
  uint32_t low = 0;
  uint32_t high = (arm_exidx_end_ - arm_exidx_start_) / sizeof(IdxHeaderData);

  IdxHeaderData hdr;

  // When set, this will be the address of the most suitable entry we find in the table. If this is
  // std::nullopt by the end of the loop below, there were no suitable matches in this module.
  std::optional<uint32_t> best_entry_addr = std::nullopt;

  // Perform an Upper Bound search to find the largest function address not greater than |pc|. At
  // the end of this loop |addr| will point to the first entry of the index whose function pointer
  // is greater than |pc|. The best match is kept separately so we can better distinguish "not
  // found" errors. Keep in mind the function addresses (the first word of the index entry) must be
  // decoded before we can use them for comparison.
  while (low + 1 < high) {
    uint32_t mid = (low + high) / 2;
    uint32_t addr = arm_exidx_start_ + mid * sizeof(IdxHeaderData);
    uint32_t prel31_encoded_offset;
    if (auto err = elf_->Read(addr, prel31_encoded_offset); err.has_err()) {
      return err;
    }

    int32_t fn_offset = DecodePrel31(prel31_encoded_offset);
    // The function offset described in the Prel31 encoding is relative to the .ARM.exidx section,
    // we have to account for the current offset into the table as well.
    uint32_t decoded_fn_addr = addr + fn_offset;

    if (pc < decoded_fn_addr) {
      high = mid;
    } else {
      low = mid;
      // This is the new best entry for this PC value. Stash the decoded function address since
      // we've already decoded it, and stash away the address of this entry so we can get the next
      // word from the header at the end.
      hdr.fn_addr = decoded_fn_addr;
      best_entry_addr = addr;
    }
  }

  if (!best_entry_addr) {
    return Error("PC not found in this module.");
  }

  // Now we can get the associated unwinding data.
  if (auto err = elf_->Read(*best_entry_addr + sizeof(hdr.fn_addr), hdr.data); err.has_err()) {
    return err;
  }

  // The high bit of the data field indicates whether bits 0-30 are an offset to the ARM.extab
  // section (which could either be the "generic model", or the "compact model" with too many
  // entries to inline into the index table) or if they're inlined opcodes (the "compact [inline]
  // model").
  if (hdr.data & 0x80000000) {
    entry.type = IdxHeader::Type::kCompactInline;
  } else {
    entry.type = IdxHeader::Type::kCompact;
    hdr.data = DecodePrel31(hdr.data);
  }

  entry.header = hdr;

  return Success();
}

Error ArmEhAbiModule::Step(Memory* stack, const Registers& current, Registers& next) {
  uint64_t pc;
  if (auto err = current.GetPC(pc); err.has_err()) {
    return err;
  }

  IdxHeader entry;
  if (auto err = Search(static_cast<uint32_t>(pc), entry); err.has_err()) {
    return err;
  }

  ArmEhAbiParser parser(entry);

  return parser.Step(stack, current, next);
}

}  // namespace unwinder
