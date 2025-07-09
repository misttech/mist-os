// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_CFI_MODULE_H_
#define SRC_LIB_UNWINDER_CFI_MODULE_H_

#include <elf.h>

#include <cstdint>
#include <map>

#include "src/lib/unwinder/cfi_parser.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

enum UnwindTableSectionType {
  // The .eh_frame section. This section conforms to the specification found at
  // https://refspecs.linuxfoundation.org/LSB_5.0.0/LSB-Core-generic/LSB-Core-generic/ehframechpt.html.
  // This section may be found in live processes (it is an allocated section) or in a stripped
  // and/or unstripped binary. It is possible for this section to also be found in split debug
  // info binaries.
  kEhFrame = 1,
  // Indicates that this is the .debug_frame section. This is a debug section and conforms to the
  // specification found here: http://www.dwarfstd.org/doc/DWARF5.pdf. Note that this section may be
  // compressed, and since the file reading API is abstracted from this library, it is the
  // responsibility of the backing ELF file Memory object to decompress the debug_frames section if
  // necessary. This section will always fail to load when memory is provided from a live process.
  kDebugFrame = 4,
};

// Represents the Call Frame Information (CFI) from the .eh_frame and/or the .debug_frame section
// of one ELF module.
//
// This class doesn't cache the memory so if repeated lookups are required, it's recommended to use
// a cached Memory implementation.
class CfiModule {
 public:
  // Caller must ensure elf to outlive us.
  CfiModule(Memory* elf, uint64_t elf_ptr, const Module& module)
      : elf_(elf), elf_ptr_(elf_ptr), address_mode_(module.mode), address_size_(module.size) {}

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
  [[nodiscard]] Error BinarySearchEhFrame(uint64_t pc, DwarfCie& cie, DwarfFde& fde);
  [[nodiscard]] Error LinearSearchEhFrame(uint64_t pc, DwarfCie& cie, DwarfFde& fde);

  // Search for CIE and FDE in .debug_frame section.
  [[nodiscard]] Error SearchDebugFrame(uint64_t pc, DwarfCie& cie, DwarfFde& fde);
  [[nodiscard]] Error BuildDebugFrameMap();

  // Reads and caches the program headers corresponding with this ELF header, doing necessary
  // upcasting for 32 bit binaries if needed.
  Error LoadPhdrs(const Elf64_Ehdr& ehdr);

  // Finds the eh_frame_hdr segment in |phdrs_| and loads the address and metadata of the binary
  // search table, as well as marking the beginning and end of the executable regions over all
  // program headers.
  Error LoadEhFrameHdr(const Elf64_Ehdr& ehdr);

  // Both of these functions read the ELF file header locally to avoid needing to include elf.h here
  // which causes compilation issues on macos.
  [[nodiscard]] Error LoadEhFrame(const Elf64_Ehdr& ehdr);

  [[nodiscard]] Error LoadDebugFrame(const Elf64_Ehdr& ehdr);

  // Helpers to decode CIE and FDE in either the eh_frame or debug_frame sections. |type|
  // determines the exact decoding details based on the section.
  [[nodiscard]] Error DecodeFde(UnwindTableSectionType type, uint64_t fde_ptr, DwarfCie& cie,
                                DwarfFde& fde);
  [[nodiscard]] Error DecodeCie(UnwindTableSectionType type, uint64_t cie_ptr, DwarfCie& cie);

  // Inputs. Use const to prevent accidental modification.
  Memory* const elf_;
  const uint64_t elf_ptr_;
  const Module::AddressMode address_mode_;
  const Module::AddressSize address_size_;

  // Marks the executable section so that we don't need to find the FDE to know a PC is wrong.
  uint64_t pc_begin_ = 0;  // inclusive
  uint64_t pc_end_ = 0;    // exclusive

  // Marks the beginning of the eh_frame section within some segment of the loaded program. Requires
  // section info to not be stripped from the binary.
  uint64_t eh_frame_begin_ = 0;

  // .eh_frame_hdr binary search table info.
  uint64_t eh_frame_hdr_ptr_ = 0;
  uint64_t fde_count_ = 0;         // Number of entries in the binary search table.
  uint64_t table_ptr_ = 0;         // Pointer to the binary search table.
  uint8_t table_enc_ = 0;          // Encoding for pointers in the table.
  uint64_t table_entry_size_ = 0;  // Size of each entry in the table.

  // .debug_frame info.
  uint64_t debug_frame_ptr_ = 0;
  uint64_t debug_frame_end_ = 0;

  std::vector<Elf64_Phdr> phdrs_;

  std::unique_ptr<CfiParser> cfi_parser_;

  // Binary search table for .debug_frame, similar to .eh_frame_hdr.
  // To save space, we only store the mapping from pc to the start of FDE.
  std::map<uint64_t, uint64_t> debug_frame_map_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_CFI_MODULE_H_
