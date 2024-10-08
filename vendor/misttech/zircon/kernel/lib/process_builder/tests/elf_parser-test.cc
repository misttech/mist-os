// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/constants.h>
#include <lib/process_builder/elf_parser.h>
#include <lib/unittest/unittest.h>

#include <vm/vm_object_paged.h>

#ifdef __x86_64__
#include "data/elf_x86-64_file-header.h"
#endif

namespace unit_testing {

namespace {

#ifdef __x86_64__
const auto& HEADER_DATA = HEADER_DATA_X86_64;
#endif

bool test_parse_file_header() {
  BEGIN_TEST;

  fbl::RefPtr<VmObjectPaged> vmo;
  VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, PAGE_SIZE, &vmo);
  vmo->Write(HEADER_DATA, 0, sizeof(HEADER_DATA));

  auto headers = process_builder::Elf64Headers::from_vmo(vmo);

  auto expected = elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr{
      // ElfIdent
      .magic = elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr::kMagic,
      .elfclass = elfldltl::ElfClass::k64,
      .elfdata = elfldltl::ElfData::k2Lsb,
      .ident_version = elfldltl::ElfVersion::kCurrent,
      .osabi = 0,
      .abiversion = 0,
      .ident_pad = {0, 0, 0, 0, 0, 0, 0},
      // ElfIdent end
      .type = elfldltl::ElfType::kDyn,
      .machine = elfldltl::ElfMachine::kNative,
      .version = elfldltl::ElfVersion::kCurrent,
      .entry = 0x10000,
      .phoff = 0,
      .shoff = 0,
      .flags = 0,
      .ehsize = sizeof(elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr),
      .phentsize = sizeof(elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr),
      .phnum = 0,
      .shentsize = 0,
      .shnum = 0,
      .shstrndx = 0,
  };

  auto actual = headers->file_header();

  ASSERT_BYTES_EQ(reinterpret_cast<uint8_t*>(&expected), reinterpret_cast<uint8_t*>(&actual),
                  sizeof(expected));
  ASSERT_EQ(0u, headers->program_headers().size());

  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_elf_parser)
UNITTEST("test parse file header", unit_testing::test_parse_file_header)
UNITTEST_END_TESTCASE(mistos_elf_parser, "mistos_elf_parser", "Tests Elf Parser")
