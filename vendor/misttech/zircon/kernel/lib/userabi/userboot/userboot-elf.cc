// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "userboot-elf.h"

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#include <lib/elfldltl/zircon.h>
#include <zircon/processargs.h>

#include <cstdint>
#include <optional>
#include <string_view>

#include "bootfs.h"
#include "util.h"

namespace {

#define INTERP_PREFIX "lib"

constexpr size_t kMaxSegments = 4;
constexpr size_t kMaxPhdrs = 16;

zx_vaddr_t load(const zx::debuglog& log, std::string_view what, const zx::vmar& vmar,
                const zx::vmo& vmo, uintptr_t* interp_off, size_t* interp_len,
                zx::vmar* segments_vmar, size_t* stack_size, bool return_entry, LoadedElf* info) {
  auto diag = elfldltl::Diagnostics(
      elfldltl::PrintfDiagnosticsReport([&log](auto&&... args) { printl(log, args...); },
                                        "userboot: ", what, ": "),
      elfldltl::DiagnosticsPanicFlags());

  elfldltl::UnownedVmoFile file(vmo.borrow(), diag);
  auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
      diag, file, elfldltl::FixedArrayFromFile<elfldltl::Elf<>::Phdr, kMaxPhdrs>());
  ZX_ASSERT(headers);
  auto& [ehdr, phdrs_result] = *headers;
  cpp20::span<const elfldltl::Elf<>::Phdr> phdrs = phdrs_result;

  std::optional<size_t> stack;
  std::optional<elfldltl::Elf<>::Phdr> interp;
  elfldltl::RemoteVmarLoader loader{vmar};
  elfldltl::LoadInfo<elfldltl::Elf<>, elfldltl::StaticVector<kMaxSegments>::Container> load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader.page_size()),
                                  elfldltl::PhdrInterpObserver<elfldltl::Elf<>>(interp),
                                  elfldltl::PhdrStackObserver<elfldltl::Elf<>>(stack)));

  if (interp_off && interp) {
    *interp_off = interp->offset;
    *interp_len = interp->filesz;
    return 0;
  }

  if (stack_size && stack) {
    *stack_size = *stack;
  }

  ZX_ASSERT(loader.Load(diag, load_info, vmo.borrow()));

  const uintptr_t entry = ehdr.entry + loader.load_bias();
  const uintptr_t base = load_info.vaddr_start() + loader.load_bias();

  using RelroRegion = decltype(load_info)::Region;
  zx::vmar loaded_vmar = std::move(loader).Commit(RelroRegion{}).TakeVmar();
  if (segments_vmar) {
    *segments_vmar = std::move(loaded_vmar);
  }

  char vmo_name[ZX_MAX_NAME_LEN];
  zx_status_t status = vmo.get_property(ZX_PROP_NAME, vmo_name, sizeof(vmo_name));
  check(log, status, "zx_object_get_property failed for ZX_PROP_NAME on vDSO VMO");

  printl(log, "userboot: loaded %.*s (%.*s) at %p, entry point %p\n", static_cast<int>(what.size()),
         what.data(), static_cast<int>(sizeof(vmo_name)), vmo_name, (void*)base, (void*)entry);

  if (info) {
    *info = LoadedElf{ehdr, zx::vmo(), base, loader.load_bias()};
  }

  return return_entry ? entry : base;
}

}  // namespace

zx_vaddr_t elf_load_vdso(const zx::debuglog& log, const zx::vmar& vmar, const zx::vmo& vmo) {
  return load(log, "vDSO", vmar, vmo, NULL, NULL, NULL, NULL, false, NULL);
}

zx_vaddr_t elf_load_bootfs(const zx::debuglog& log, Bootfs& bootfs, std::string_view root,
                           const zx::process& proc, const zx::vmar& vmar, const zx::thread& thread,
                           std::string_view filename, size_t* stack_size, ElfInfo* info) {
  zx::vmo vmo = bootfs.Open(root, filename, "program");

  uintptr_t interp_off = 0;
  size_t interp_len = 0;
  zx_vaddr_t entry = load(log, filename, vmar, vmo, &interp_off, &interp_len, NULL, stack_size,
                          true, &info->main_elf);
  if (interp_len > 0) {
    // While PT_INTERP names can be arbitrarily large, bootfs entries
    // have names of bounded length.
    constexpr size_t kInterpMaxLen = ZBI_BOOTFS_MAX_NAME_LEN;
    constexpr size_t kInterpPrefixLen = sizeof(INTERP_PREFIX) - 1;
    static_assert(kInterpMaxLen >= kInterpPrefixLen);
    constexpr size_t kInterpSuffixLen = kInterpMaxLen - kInterpPrefixLen;

    if (interp_len > kInterpSuffixLen) {
      return ZX_ERR_INVALID_ARGS;
    }

    // Add one for the trailing nul.
    char interp[kInterpMaxLen + 1];

    // Copy the prefix.
    memcpy(interp, INTERP_PREFIX, kInterpPrefixLen);

    // Copy the suffix.
    zx_status_t status = vmo.read(&interp[kInterpPrefixLen], interp_off, interp_len);
    if (status != ZX_OK)
      fail(log, "zx_vmo_read failed: %d", status);

    // Copy the nul.
    interp[kInterpPrefixLen + interp_len] = '\0';

    printl(log, "'%.*s' has PT_INTERP \"%s\"", static_cast<int>(filename.size()), filename.data(),
           interp);

    zx::vmo interp_vmo = bootfs.Open(root, interp, "dynamic linker");
    zx::vmar interp_vmar;
    entry = load(log, interp, vmar, interp_vmo, NULL, NULL, &interp_vmar, NULL, true,
                 &info->interp_elf);

    // info->main_elf.vmo = std::move(vmo);
    info->has_interp = true;
  }
  return entry;
}
