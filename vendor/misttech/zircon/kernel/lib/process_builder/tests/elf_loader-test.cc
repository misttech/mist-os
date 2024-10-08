// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/testing/unittest.h>
#include <lib/process_builder/elf_loader.h>
#include <lib/process_builder/elf_parser.h>
#include <lib/unittest/unittest.h>
#include <string.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/array.h>
#include <vm/vm_object_paged.h>

#include "fbl/array.h"
#include "lib/elfldltl/load.h"
#include "lib/elfldltl/static-vector.h"
#include "object/vm_object_dispatcher.h"

namespace unit_testing {

namespace {

constexpr ktl::string_view a_long_vmo_name_prefix = "a_long_vmo_name_prefix:";
constexpr ktl::string_view a_great_maximum_length_vmo_name = "a_great_maximum_length_vmo_name";
constexpr ktl::string_view anystringhere = "anystringhere";

#if 0
auto GetDiagnostics() {
  return elfldltl::Diagnostics(elfldltl::PrintfDiagnosticsReport(
                                   [](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
                                     printf(std::forward<decltype(args)>(args)...);
#pragma GCC diagnostic pop
                                   },
                                   "elf loader: "),
                               elfldltl::DiagnosticsPanicFlags());
}
#endif

bool test_vmo_name_with_prefix() {
  BEGIN_TEST;

  ktl::string_view empty_vmo_name;
  ktl::string_view short_vmo_name("short_vmo_name");
  ktl::string_view max_vmo_name("a_great_maximum_length_vmo_name");

  ASSERT_STREQ(
      "bss:<unknown ELF>",
      process_builder::vmo_name_with_prefix<process_builder::kVmoNamePrefixBss>(empty_vmo_name));

  ASSERT_STREQ(
      "bss:short_vmo_name",
      process_builder::vmo_name_with_prefix<process_builder::kVmoNamePrefixBss>(short_vmo_name));

  ASSERT_STREQ(
      "bss:a_great_maximum_length_vmo_",
      process_builder::vmo_name_with_prefix<process_builder::kVmoNamePrefixBss>(max_vmo_name));

  ASSERT_STREQ(
      "data:a_great_maximum_length_vmo",
      process_builder::vmo_name_with_prefix<process_builder::kVmoNamePrefixData>(max_vmo_name));

  ASSERT_STREQ(
      "a_long_vmo_name_prefix:<unknown",
      process_builder::vmo_name_with_prefix<a_long_vmo_name_prefix>(empty_vmo_name),
      process_builder::vmo_name_with_prefix<a_long_vmo_name_prefix>(empty_vmo_name).data());

  ASSERT_STREQ(max_vmo_name, process_builder::vmo_name_with_prefix<a_great_maximum_length_vmo_name>(
                                 empty_vmo_name));

  ASSERT_STREQ("anystringherea_great_maximum_le",
               process_builder::vmo_name_with_prefix<anystringhere>(max_vmo_name));

  END_TEST;
}

struct RecordedMapping {
  fbl::RefPtr<VmObject> vmo;
  uint64_t vmo_offset;
  size_t length;
  zx_vm_option_t flags;
};

class TrackingMapper : public process_builder::Mapper {
 public:
  fit::result<zx_status_t, size_t> map(size_t vmar_offset, fbl::RefPtr<VmObjectDispatcher> vmo,
                                       uint64_t vmo_offset, size_t length,
                                       zx_vm_option_t flags) final {
    fbl::AllocChecker ac;
    printf("map(%p):vmar_offset %zu vmo_offset %lu length %zu flags 0x%x\n", vmo->vmo().get(),
           vmar_offset, vmo_offset, length, flags);
    mappings_.push_back(
        {.vmo = vmo->vmo(), .vmo_offset = vmo_offset, .length = length, .flags = flags}, &ac);
    ASSERT(ac.check());
    return fit::ok(vmar_offset);
  }

  const fbl::Vector<RecordedMapping>& mappings() const { return mappings_; }

 private:
  fbl::Vector<RecordedMapping> mappings_;
};

constexpr elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr ELF_FILE_HEADER{
    // ElfIdent
    .magic = elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr::kMagic,
    .elfclass = elfldltl::ElfClass::k64,
    .elfdata = elfldltl::ElfData::kNative,
    .ident_version = elfldltl::ElfVersion::kCurrent,
    .osabi = 0,
    .abiversion = 0,
    .ident_pad = {0, 0, 0, 0, 0, 0, 0},
    // ElfIdent end
    .type = elfldltl::ElfType::kDyn,
    .machine = elfldltl::ElfMachine::kNative,
    .version = elfldltl::ElfVersion::kCurrent,
    .entry = 0x10000,
    .phoff = sizeof(elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr),
    .shoff = 0,
    .flags = 0,
    .ehsize = static_cast<uint16_t>(sizeof(elfldltl::Elf<elfldltl::ElfClass::k64>::Ehdr)),
    .phentsize = static_cast<uint16_t>(sizeof(elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr)),
    .phnum = 1,
    .shentsize = 0,
    .shnum = 0,
    .shstrndx = 0,
};

}  // namespace

bool map_read_only_with_page_unaligned_bss() {
  BEGIN_TEST;
  fbl::AllocChecker ac;
  constexpr ktl::string_view ELF_DATA = "FUCHSIA!";

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = static_cast<uint64_t>(ELF_DATA.size()),
      .memsz = 0x100,
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  // Contains a PT_LOAD segment where the filesz is less than memsz (BSS).
  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  printf("vmo %p\n", vmo.get());

  // Fill the VMO with 0xff, so that we can verify that the BSS section is correctly zeroed.
  fbl::Array<uint8_t> pattern = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE * 2ul);
  ASSERT_TRUE(ac.check());
  memset(pattern.data(), 0xFF, pattern.size());
  ASSERT_OK(vmo->Write(pattern.data(), 0, pattern.size()), "fill VMO with 0xff");
  // Write the PT_LOAD segment's data at the defined offset.
  ASSERT_OK(vmo->Write(ELF_DATA.data(), PAGE_SIZE, ELF_DATA.size()), "write data to VMO");

  TrackingMapper mapper;
#if 1
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");
#else
  auto diag = GetDiagnostics();
  // auto mapper_relative_bias = vaddr_bias - mapper_base;

  elfldltl::LoadInfo<elfldltl::Elf<elfldltl::ElfClass::k64>,
                     elfldltl::StaticVector<process_builder::kMaxSegments>::Container>
      load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(diag, headers.program_headers(),
                                  load_info.GetPhdrObserver(PAGE_SIZE, false)));
  process_builder::RemoteLoader loader(mapper);
  ASSERT_TRUE(loader.Load(diag, load_info, handle.dispatcher()), "map ELF segments");

#endif

  auto mapping_iter = mapper.mappings().begin();

  // Extract the VMO and offset that was supposed to be mapped.
  ASSERT_TRUE(mapping_iter != mapper.mappings().end(), "mapping from ELF VMO");
  auto mapping = mapping_iter;

  // Read a page of data that was "mapped".
  fbl::Array<uint8_t> data = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  ASSERT_OK(mapping->vmo->Read(data.data(), mapping->vmo_offset, data.size()), "read VMO");

  // Construct the expected memory, which is ASCII "FUCHSIA!" followed by 0s for the rest of
  // the page.
  fbl::Array<uint8_t> expected = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  memcpy(expected.data(), ELF_DATA.data(), ELF_DATA.size());

  ASSERT_BYTES_EQ(expected.data(), data.data(), expected.size());

  // No more mappings expected.
  ASSERT_TRUE(++mapping == mapper.mappings().end());
  END_TEST;
}

bool map_read_only_vmo_with_page_aligned_bss() {
  BEGIN_TEST;
  fbl::AllocChecker ac;

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = static_cast<uint64_t>(PAGE_SIZE),
      .memsz = static_cast<uint64_t>(PAGE_SIZE) * 2,
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  // Contains a PT_LOAD segment where the filesz is less than memsz (BSS).
  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  printf("vmo %p\n", vmo.get());

  // Fill the VMO with 0xff, so that we can verify that the BSS section is correctly zeroed.
  fbl::Array<uint8_t> pattern = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE * 2ul);
  ASSERT_TRUE(ac.check());
  memset(pattern.data(), 0xFF, pattern.size());
  ASSERT_OK(vmo->Write(pattern.data(), 0, pattern.size()), "fill VMO with 0xff");

  TrackingMapper mapper;
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");

  auto mapping_iter = mapper.mappings().begin();

  // Verify that a COW VMO was not created, since we didn't need to write to the original VMO.
  // We must check that KOIDs are the same, since we duplicate the handle when recording it
  // in TrackingMapper.
  ASSERT_TRUE(mapping_iter != mapper.mappings().end(), "mapping from ELF VMO");
  auto mapping = mapping_iter;

  // ASSERT_TRUE(vmo->user_id() == mapping->vmo->user_id());

  fbl::Array<uint8_t> data = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());

  // Ensure the first page is from the ELF.
  ASSERT_OK(mapping->vmo->Read(data.data(), mapping->vmo_offset, data.size()), "read ELF VMO");
  ASSERT_BYTES_EQ(pattern.data(), data.data(), data.size());

  ASSERT_TRUE(++mapping_iter != mapper.mappings().end(), "mapping from BSS VMO");
  mapping = mapping_iter;

  // Ensure the second page is BSS.
  ASSERT_OK(mapping->vmo->Read(data.data(), mapping->vmo_offset, data.size()), "read BSS VMO");
  fbl::Array<uint8_t> zero = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());
  ASSERT_BYTES_EQ(zero.data(), data.data(), zero.size());

  // No more mappings expected.
  ASSERT_TRUE(++mapping == mapper.mappings().end());
  END_TEST;
}

bool map_read_only_vmo_with_no_bss() {
  BEGIN_TEST;
  fbl::AllocChecker ac;

  // Contains a PT_LOAD segment where there is no BSS.
  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kExecute,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = static_cast<uint64_t>(PAGE_SIZE),
      .memsz = static_cast<uint64_t>(PAGE_SIZE),
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  // Fill the VMO with 0xff, so we can verify the BSS section is correctly allocated.
  fbl::Array<uint8_t> pattern = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE * 2ul);
  ASSERT_TRUE(ac.check());
  memset(pattern.data(), 0xFF, pattern.size());
  ASSERT_OK(vmo->Write(pattern.data(), 0, pattern.size()), "fill VMO with 0xff");

  TrackingMapper mapper;
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");

  auto mapping_iter = mapper.mappings().begin();

  // Verify that a COW VMO was not created, since we didn't need to write to the original VMO.
  // We must check that KOIDs are the same, since we duplicate the handle when recording it
  // in TrackingMapper.
  ASSERT_TRUE(mapping_iter != mapper.mappings().end(), "mapping from ELF VMO");
  auto mapping = mapping_iter;

  ASSERT_TRUE(vmo == mapping->vmo);

  fbl::Array<uint8_t> data = fbl::MakeArray<uint8_t>(&ac, PAGE_SIZE);
  ASSERT_TRUE(ac.check());

  // Ensure the first page is from the ELF.
  ASSERT_OK(mapping->vmo->Read(data.data(), mapping->vmo_offset, data.size()), "read ELF VMO");
  ASSERT_BYTES_EQ(pattern.data(), data.data(), data.size());

  // No more mappings expected.
  ASSERT_TRUE(++mapping == mapper.mappings().end());
  END_TEST;
}

bool map_read_only_vmo_with_write_flag() {
  BEGIN_TEST;

  // Contains a PT_LOAD segment where there is no BSS.
  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = static_cast<uint64_t>(PAGE_SIZE),
      .memsz = static_cast<uint64_t>(PAGE_SIZE),
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  TrackingMapper mapper;
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");

  auto mapping_iter = mapper.mappings().begin();

  // Verify that a COW VMO was created, since the segment had a WRITE flag.
  // We must check that are different, since we duplicate the handle when recording it
  // in TrackingMapper.
  ASSERT_TRUE(mapping_iter != mapper.mappings().end(), "mapping from ELF VMO");
  auto mapping = mapping_iter;

  ASSERT_TRUE(vmo != mapping->vmo);

  // Attempt to write to the VMO.
  constexpr ktl::string_view ELF_DATA = "FUCHSIA!";
  ASSERT_OK(mapping->vmo->Write(ELF_DATA.data(), mapping->vmo_offset, ELF_DATA.size()),
            "write to COW VMO");

  // No more mappings expected.
  ASSERT_TRUE(++mapping == mapper.mappings().end());
  END_TEST;
}

bool segment_with_zero_file_size() {
  BEGIN_TEST;
  // Contains a PT_LOAD segment whose filesz is 0.
  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = 0,
      .memsz = 1,
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  TrackingMapper mapper;
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");

  for (auto& mapping : mapper.mappings()) {
    ASSERT_TRUE(mapping.length != 0);
  }

  END_TEST;
}

bool map_execute_only_segment() {
  BEGIN_TEST;
  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr ELF_PROGRAM_HEADER{
      .type = elfldltl::ElfPhdrType::kLoad,
      .flags = elfldltl::PhdrBase::kExecute,
      .offset = PAGE_SIZE,
      .vaddr = 0x10000,
      .paddr = 0x10000,
      .filesz = 0x10,
      .memsz = 0x10,
      .align = static_cast<uint64_t>(PAGE_SIZE)};

  elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr phdrs[] = {ELF_PROGRAM_HEADER};

  auto headers = process_builder::Elf64Headers::new_for_test(
      ELF_FILE_HEADER, ktl::optional<ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>(
                           ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>(phdrs)));

  KernelHandle<VmObjectDispatcher> handle;
  // zx::Vmo::create
  {
    zx_rights_t rights;
    fbl::RefPtr<VmObjectPaged> tmp_vmo;
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, PAGE_SIZE * 2ul, &tmp_vmo);
    ZX_ASSERT(status == ZX_OK);
    status = VmObjectDispatcher::Create(ktl::move(tmp_vmo), PAGE_SIZE * 2ul,
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
  }
  fbl::RefPtr<VmObject> vmo = handle.dispatcher()->vmo();

  TrackingMapper mapper;
  ASSERT_TRUE(
      process_builder::map_elf_segments(handle.dispatcher(), headers, &mapper, 0, 0).is_ok(),
      "map ELF segments");

  auto mapping_iter = mapper.mappings().begin();
  auto mapping = mapping_iter;
  ASSERT_EQ(mapping->flags, ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS | ZX_VM_PERM_EXECUTE |
                                ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED);

  // No more mappings expected.
  ASSERT_TRUE(++mapping == mapper.mappings().end());
  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_elf_loader)
UNITTEST("test vmo name with prefix", unit_testing::test_vmo_name_with_prefix)
UNITTEST("map read/write with page unaligned bss",
         unit_testing::map_read_only_with_page_unaligned_bss)
UNITTEST("map read/write vmo with page aligned bss",
         unit_testing::map_read_only_vmo_with_page_aligned_bss)
UNITTEST("map read only vmo with no bss", unit_testing::map_read_only_vmo_with_no_bss)
UNITTEST("map read only vmo with write flag", unit_testing::map_read_only_vmo_with_write_flag)
UNITTEST("segment with zero file size", unit_testing::segment_with_zero_file_size)
UNITTEST("map execute only segment", unit_testing::map_execute_only_segment)
UNITTEST_END_TESTCASE(mistos_elf_loader, "mistos_elf_loader", "Tests Elf Loader")
