// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/process_builder/elf_parser.h"

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/static-vector.h>
#include <sys/types.h>

#include <utility>

#include <ktl/optional.h>

using off_t = uint64_t;

#include "vmo_file.h"

namespace process_builder {

namespace {

auto GetDiagnostics() {
  return elfldltl::Diagnostics(elfldltl::PrintfDiagnosticsReport(
                                   [](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
                                     printf(std::forward<decltype(args)>(args)...);
#pragma GCC diagnostic pop
                                   },
                                   "elf parser: "),
                               elfldltl::DiagnosticsPanicFlags());
}

}  // namespace

fit::result<ElfParseError, Elf64Headers> Elf64Headers::from_vmo(const fbl::RefPtr<VmObject>& vmo) {
  auto diagnostics = GetDiagnostics();
  process_builder::VmoFile vmo_file(vmo, diagnostics);
  auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<elfldltl::ElfClass::k64>>(
      diagnostics, vmo_file,
      elfldltl::FixedArrayFromFile<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr, kMaxPhdrs>());

  ASSERT(headers);
  auto& [ehdr, phdrs_result] = *headers;

  return fit::ok(Elf64Headers(ehdr, ktl::move(phdrs_result)));
}

/// Returns 0 or 1 headers of the given type, or Err([ElfParseError::MultipleHeaders]) if more
/// than 1 such header is present.
fit::result<ElfParseError, ktl::optional<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr>>
Elf64Headers::program_header_with_type(elfldltl::ElfPhdrType type) {
  elfldltl::LoadInfo<elfldltl::Elf<elfldltl::ElfClass::k64>,
                     elfldltl::StaticVector<kMaxSegments>::Container>
      load_info;

  switch (type) {
    case elfldltl::ElfPhdrType::kInterp: {
      ktl::optional<elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr> interp;
      auto sucess = elfldltl::DecodePhdrs(GetDiagnostics(), *program_headers_,
                                          load_info.GetPhdrObserver(PAGE_SIZE),
                                          elfldltl::PhdrInterpObserver<elfldltl::Elf<>>(interp));
      if (sucess) {
        return fit::ok(interp);
      }
      return fit::error(ElfParseError{});
    }
    case elfldltl::ElfPhdrType::kNull:
    case elfldltl::ElfPhdrType::kLoad:
    case elfldltl::ElfPhdrType::kDynamic:
    case elfldltl::ElfPhdrType::kNote:
    case elfldltl::ElfPhdrType::kPhdr:
    case elfldltl::ElfPhdrType::kTls:
    case elfldltl::ElfPhdrType::kEhFrameHdr:
    case elfldltl::ElfPhdrType::kStack:
    case elfldltl::ElfPhdrType::kRelro:
      break;
  }

  return fit::error(ElfParseError{});
}

}  // namespace process_builder
