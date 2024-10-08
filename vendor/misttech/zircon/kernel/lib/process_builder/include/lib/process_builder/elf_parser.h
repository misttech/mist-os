// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_PARSER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_PARSER_H_

#include <lib/elfldltl/constants.h>
#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/phdr.h>
#include <lib/fit/result.h>
#include <lib/mistos/util/bitflags.h>

#include <ktl/optional.h>
#include <vm/vm_object.h>

#include "lib/elfldltl/memory.h"
#include "zircon/assert.h"

namespace unit_testing {
bool map_read_only_with_page_unaligned_bss();
bool map_read_only_vmo_with_page_aligned_bss();
bool map_read_only_vmo_with_no_bss();
bool map_read_only_vmo_with_write_flag();
bool segment_with_zero_file_size();
bool map_execute_only_segment();
}  // namespace unit_testing

namespace process_builder {

constexpr size_t kMaxSegments = 4;
constexpr size_t kMaxPhdrs = 16;

struct ElfParseError {};

class Elf64Headers {
 public:
  static fit::result<ElfParseError, Elf64Headers> from_vmo(const fbl::RefPtr<VmObject>& vmo);

  elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr file_header() const { return file_header_; }

  ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr> program_headers() const {
    if (program_headers_.has_value()) {
      return program_headers_.value();
    }
    ZX_PANIC("Empty Program Headers");
  }

  fit::result<ElfParseError, ktl::optional<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>
  program_header_with_type(elfldltl::ElfPhdrType type);

 private:
  friend bool unit_testing::map_read_only_with_page_unaligned_bss();
  friend bool unit_testing::map_read_only_vmo_with_page_aligned_bss();
  friend bool unit_testing::map_read_only_vmo_with_no_bss();
  friend bool unit_testing::map_read_only_vmo_with_write_flag();
  friend bool unit_testing::segment_with_zero_file_size();
  friend bool unit_testing::map_execute_only_segment();

  Elf64Headers(
      elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr file_header,
      ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>> program_headers)
      : file_header_(file_header), program_headers_(program_headers) {}

  Elf64Headers(elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr file_header,
               elfldltl::FixedArrayFromFile<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr,
                                            kMaxPhdrs>::Result phdrs)
      : file_header_(file_header), phdrs_(ktl::move(phdrs)) {
    ZX_ASSERT(phdrs_);
    program_headers_ = phdrs_;
  }

  /// Creates an instance of Elf64Headers from in-memory representations of the ELF headers.
  static Elf64Headers new_for_test(
      elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr file_header,
      ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>> program_header) {
    return Elf64Headers(file_header, program_header);
  }

  elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr file_header_;

  elfldltl::FixedArrayFromFile<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr, kMaxPhdrs>::Result
      phdrs_;

  ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>> program_headers_;
};

}  // namespace process_builder

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_PARSER_H_
