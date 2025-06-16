// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/note.h>
#include <lib/fit/defer.h>
#include <lib/zxdump/elf-search.h>

#include <array>
#include <cinttypes>
#include <cstdint>
#include <initializer_list>
#include <ostream>
#include <string>
#include <tuple>
#include <vector>

#include <gmock/gmock.h>

#include "dump-tests.h"
#include "test-file.h"

namespace std {

std::ostream& operator<<(std::ostream& os, const std::vector<std::byte>& bytes) {
  for (std::byte byte : bytes) {
    char buf[3];
    snprintf(buf, sizeof(buf), "%02x", static_cast<unsigned int>(byte));
    os << buf;
  }
  return os;
}

}  // namespace std

namespace zxdump::testing {

using ::testing::AllOf;
using ::testing::Contains;
using ::testing::Field;
using ::testing::IsEmpty;
using ::testing::Not;

using ByteVector = std::vector<std::byte>;

struct ElfId {
  ByteVector build_id;
  std::string soname;
  uint64_t base = 0;
};

std::ostream& operator<<(std::ostream& os, const ElfId& id) {
  return os << "{ build ID: " << id.build_id << ", SONAME: \"" << id.soname << "\", 0x" << std::hex
            << id.base << " }";
}

auto MakeElfId(std::initializer_list<uint8_t> id, std::string_view soname) {
  ByteVector bytes;
  for (uint8_t byte : id) {
    bytes.push_back(static_cast<std::byte>(byte));
  }
  return ElfId{std::move(bytes), std::string(soname)};
}

// The generated .inc file has `MakeElfId({0x...,...}, "soname"),` lines.
const std::array kElfSearchIds = {
#include "test-child-elf-search.inc"
};

auto ElfIdEq(const ElfId& id, bool use_soname) {
  return AllOf(Field("build ID", &ElfId::build_id, id.build_id),
               use_soname ? Field("SONAME", &ElfId::soname, id.soname)
                          : Field("SONAME", &ElfId::soname, IsEmpty()));
}

auto HasElfSearchIds(bool use_soname) {
  auto has_ids = [use_soname](auto... ids) {
    return AllOf(Contains(ElfIdEq(ids, use_soname)).Times(1)...);
  };
  return std::apply(has_ids, kElfSearchIds);
}

// This is called before dumping starts to make callbacks.
void TestProcessForElfSearch::Precollect(zxdump::TaskHolder& holder, zxdump::ProcessDump& dump) {
  dump_ = &dump;
}

// This is the callback made from DumpMemory for each segment.
// It needs the dumper pointer saved in Precollect.
fit::result<zxdump::Error, zxdump::SegmentDisposition>
TestProcessForElfSearch::DumpAllMemoryWithBuildIds(zxdump::SegmentDisposition segment,
                                                   const zx_info_maps_t& maps,
                                                   const zx_info_vmo_t& vmo) {
  if (segment.filesz > 0 && zxdump::IsLikelyElfMapping(maps)) {
    auto result = dump_->FindBuildIdNote(maps);
    if (result.is_error()) {
      return result.take_error();
    }
    segment.note = result.value();
  }
  return fit::ok(segment);
}

void TestProcessForElfSearch::StartChild() {
  SpawnAction({
      .action = FDIO_SPAWN_ACTION_SET_NAME,
      .name = {kChildName},
  });

  fbl::unique_fd read_pipe;
  {
    int pipe_fd[2];
    ASSERT_EQ(0, pipe(pipe_fd)) << strerror(errno);
    read_pipe.reset(pipe_fd[STDIN_FILENO]);
    SpawnAction({
        .action = FDIO_SPAWN_ACTION_TRANSFER_FD,
        .fd = {.local_fd = pipe_fd[STDOUT_FILENO], .target_fd = STDOUT_FILENO},
    });
  }

  ASSERT_NO_FATAL_FAILURE(TestProcess::StartChild({"-d", "-D"}));

  // The test-child wrote the pointers dladdr returned as module base addresses
  // for the main and DSO symbols.  Reading these immediately synchronizes with
  // the child having started up and progressed far enough to have finished all
  // its loading before the process gets dumped.
  FILE* pipef = fdopen(read_pipe.get(), "r");
  ASSERT_TRUE(pipef) << "fdopen: " << read_pipe.get() << strerror(errno);
  auto close_pipef = fit::defer([pipef]() { fclose(pipef); });
  std::ignore = read_pipe.release();

  ASSERT_EQ(2, fscanf(pipef, "%" SCNx64 "\n%" SCNx64, &main_ptr_, &dso_ptr_));
}

void TestProcessForElfSearch::CheckDump(zxdump::TaskHolder& holder) {
  auto find_result = holder.root_job().find(koid());
  ASSERT_TRUE(find_result.is_ok()) << find_result.error_value();

  ASSERT_EQ(find_result->get().type(), ZX_OBJ_TYPE_PROCESS);
  zxdump::Process& read_process = static_cast<zxdump::Process&>(find_result->get());

  {
    auto name_result = read_process.get_property<ZX_PROP_NAME>();
    ASSERT_TRUE(name_result.is_ok()) << name_result.error_value();
    std::string_view name(name_result->data(), name_result->size());
    name = name.substr(0, name.find_first_of('\0'));
    EXPECT_EQ(name, std::string_view(kChildName));
  }

  std::vector<ElfId> found_elf;
  auto maps = read_process.get_info<ZX_INFO_PROCESS_MAPS>();
  ASSERT_TRUE(maps.is_ok()) << maps.error_value();
  auto first = maps->begin();
  do {
    auto [next, last] = zxdump::ElfSearch(read_process, first, maps->end());
    if (next != last) {
      const zx_info_maps_t& segment = *next;
      ASSERT_EQ(segment.type, ZX_INFO_MAPS_TYPE_MAPPING);
      auto detect = zxdump::DetectElf(read_process, segment);
      ASSERT_TRUE(detect.is_ok()) << detect.error_value();
      std::span phdrs = **detect;
      EXPECT_THAT(phdrs, Not(IsEmpty()));
      auto identity = zxdump::DetectElfIdentity(read_process, segment, phdrs);
      ASSERT_TRUE(identity.is_ok()) << identity.error_value();
      EXPECT_GT(identity->build_id.size, zxdump::ElfIdentity::kBuildIdOffset);
      auto id_bytes = read_process.read_memory<std::byte, zxdump::ByteView>(
          identity->build_id.vaddr + zxdump::ElfIdentity::kBuildIdOffset,
          identity->build_id.size - zxdump::ElfIdentity::kBuildIdOffset);
      ASSERT_TRUE(id_bytes.is_ok()) << id_bytes.error_value();
      std::string soname;
      if (identity->soname.size != 0) {
        auto result = read_process.read_memory<char, std::string_view>(  //
            identity->soname.vaddr, identity->soname.size);
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        soname = **result;
      }
      found_elf.push_back({
          .build_id{id_bytes.value()->begin(), id_bytes.value()->end()},
          .soname{std::move(soname)},
          .base = segment.base,
      });
    }
    first = last;
  } while (first != maps->end());

  EXPECT_THAT(found_elf, HasElfSearchIds(true));

  // Only the main executable should have no SONAME.
  EXPECT_THAT(found_elf, Contains(Field("soname", &ElfId::soname, IsEmpty())).Times(1));

  // That module's base should be what dladdr said in the child.
  EXPECT_THAT(found_elf, Contains(AllOf(Field("soname", &ElfId::soname, IsEmpty()),
                                        Field("load address", &ElfId::base, main_ptr()))));

  // Similar pair for the DSO module.
  EXPECT_THAT(found_elf, Contains(Field(&ElfId::soname, kDsoSoname)).Times(1));
  EXPECT_THAT(found_elf, Contains(AllOf(Field("soname", &ElfId::soname, kDsoSoname),
                                        Field("load address", &ElfId::base, dso_ptr()))));
}

void TestProcessForElfSearch::CheckNotes(int fd) {
  auto must_read = [fd](auto&& span, off_t pos) {
    std::span data(span);
    ssize_t nread = pread(fd, data.data(), data.size_bytes(), pos);
    ASSERT_GE(nread, 0) << strerror(errno);
    ASSERT_EQ(data.size_bytes(), static_cast<size_t>(nread));
  };

  zxdump::Elf::Ehdr ehdr;
  ASSERT_NO_FATAL_FAILURE(must_read(std::span(&ehdr, 1), 0));

  std::vector<zxdump::Elf::Phdr> phdrs(ehdr.phnum);
  ASSERT_NO_FATAL_FAILURE(must_read(phdrs, ehdr.phoff));

  std::vector<ElfId> found_elf;
  for (const zxdump::Elf::Phdr& phdr : phdrs) {
    if (phdr.type == elfldltl::ElfPhdrType::kNote) {
      ByteVector bytes(phdr.filesz, {});
      ASSERT_NO_FATAL_FAILURE(must_read(bytes, phdr.offset));
      elfldltl::ElfNoteSegment<elfldltl::ElfData::k2Lsb> notes(bytes);
      for (const auto& note : notes) {
        if (note.IsBuildId()) {
          found_elf.push_back({
              .build_id{note.desc.begin(), note.desc.end()},
              .base = (&phdr)[-1].vaddr,
          });
        }
      }
    }
  }

  EXPECT_THAT(found_elf, HasElfSearchIds(false));

  for (const auto& module : kElfSearchIds) {
    const uint64_t base = module.soname.empty() ? main_ptr_ : dso_ptr_;
    auto base_eq = Field("load address", &ElfId::base, base);
    EXPECT_THAT(found_elf, Contains(AllOf(ElfIdEq(module, false), base_eq)));
  }
}

}  // namespace zxdump::testing

namespace {

TEST(ZxdumpTests, ElfSearchLive) {
  zxdump::testing::TestProcessForElfSearch process;
  ASSERT_NO_FATAL_FAILURE(process.StartChild());

  zxdump::TaskHolder holder;
  auto insert_result = holder.Insert(process.handle());
  ASSERT_TRUE(insert_result.is_ok()) << insert_result.error_value();

  ASSERT_NO_FATAL_FAILURE(process.CheckDump(holder));
}

TEST(ZxdumpTests, ElfSearchDump) {
  zxdump::testing::TestFile file;
  zxdump::FdWriter writer(file.RewoundFd());

  zxdump::testing::TestProcessForElfSearch process;
  ASSERT_NO_FATAL_FAILURE(process.StartChild());

  ASSERT_NO_FATAL_FAILURE(process.Dump(writer));

  zxdump::TaskHolder holder;
  auto read_result = holder.Insert(file.RewoundFd());
  ASSERT_TRUE(read_result.is_ok()) << read_result.error_value();

  ASSERT_NO_FATAL_FAILURE(process.CheckDump(holder));

  ASSERT_NO_FATAL_FAILURE(process.CheckNotes(file.RewoundFd().get()));
}

}  // namespace
