// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_CFI_MODULE_H_
#define SRC_LIB_UNWINDER_CFI_MODULE_H_

#include <cstdint>
#include <map>

#include "src/lib/unwinder/cfi_parser.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

// Represents the Call Frame Information (CFI) from the .eh_frame and/or the .debug_frame section
// of one ELF module.
//
// This class doesn't cache the memory so if repeated lookups are required, it's recommended to use
// a cached Memory implementation.
class CfiModule {
 public:
  // Caller must ensure elf to outlive us.
  CfiModule(Memory* elf, uint64_t elf_ptr, Module::AddressMode address_mode)
      : elf_(elf), elf_ptr_(elf_ptr), address_mode_(address_mode) {}

  // Load the CFI from the ELF file.
  [[nodiscard]] Error Load();

  // Unwind one frame.
  [[nodiscard]] Error Step(Memory* stack, const Registers& current, Registers& next);

  void AsyncStep(AsyncMemory* stack, const Registers& current,
                 fit::callback<void(Error, Registers)> cb);

  // Check whether a given PC is in the valid range.
  bool IsValidPC(uint64_t pc) const { return pc >= pc_begin_ && pc < pc_end_; }

  // Memory accessor.
  Memory* memory() const { return elf_; }

 private:
  // DWARF Common Information Entry.
  struct DwarfCie {
    uint64_t code_alignment_factor = 0;       // usually 1.
    int64_t data_alignment_factor = 0;        // usually -4 on arm64, -8 on x64.
    RegisterID return_address_register;       // PC on x64, LR on arm64.
    bool fde_have_augmentation_data = false;  // should always be true for .eh_frame.
    uint8_t fde_address_encoding = 0xFF;      // default to an invalid encoding.
    uint64_t instructions_begin = 0;
    uint64_t instructions_end = 0;  // exclusive.
  };

  // DWARF Frame Description Entry.
  struct DwarfFde {
    uint64_t pc_begin = 0;
    uint64_t pc_end = 0;
    uint64_t instructions_begin = 0;
    uint64_t instructions_end = 0;  // exclusive.
  };

  // Common code before using the cfi_parser to perform the actual step.
  [[nodiscard]] Error PrepareToStep(const Registers& current, DwarfCie& cie);

  // Search for CIE and FDE in .eh_frame section.
  [[nodiscard]] Error SearchEhFrame(uint64_t pc, DwarfCie& cie, DwarfFde& fde);

  // Search for CIE and FDE in .debug_frame section.
  [[nodiscard]] Error SearchDebugFrame(uint64_t pc, DwarfCie& cie, DwarfFde& fde);
  [[nodiscard]] Error BuildDebugFrameMap();

  // Both of these functions read the ELF file header locally to avoid needing to include elf.h here
  // which causes compilation issues on macos.
  [[nodiscard]] Error LoadEhFrame();
  [[nodiscard]] Error LoadDebugFrame();

  // Helpers to decode CIE and FDE. Version could be 1 or 4.
  [[nodiscard]] Error DecodeFde(uint8_t version, uint64_t fde_ptr, DwarfCie& cie, DwarfFde& fde);
  [[nodiscard]] Error DecodeCie(uint8_t version, uint64_t cie_ptr, DwarfCie& cie);

  // Inputs. Use const to prevent accidental modification.
  Memory* const elf_;
  const uint64_t elf_ptr_;
  const Module::AddressMode address_mode_;

  // Marks the executable section so that we don't need to find the FDE to know a PC is wrong.
  uint64_t pc_begin_ = 0;  // inclusive
  uint64_t pc_end_ = 0;    // exclusive

  // .eh_frame_hdr binary search table info.
  uint64_t eh_frame_hdr_ptr_ = 0;
  uint64_t fde_count_ = 0;         // Number of entries in the binary search table.
  uint64_t table_ptr_ = 0;         // Pointer to the binary search table.
  uint8_t table_enc_ = 0;          // Encoding for pointers in the table.
  uint64_t table_entry_size_ = 0;  // Size of each entry in the table.

  // .debug_frame info.
  uint64_t debug_frame_ptr_ = 0;
  uint64_t debug_frame_end_ = 0;

  std::unique_ptr<CfiParser> cfi_parser_;

  // Binary search table for .debug_frame, similar to .eh_frame_hdr.
  // To save space, we only store the mapping from pc to the start of FDE.
  std::map<uint64_t, uint64_t> debug_frame_map_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_CFI_MODULE_H_
