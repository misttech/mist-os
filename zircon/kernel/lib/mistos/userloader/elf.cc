// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/mistos/elfldltl/vmar-loader.h>
#include <lib/mistos/elfldltl/vmo.h>
#include <lib/mistos/userloader/elf.h>
#include <lib/mistos/util/random.h>
#include <lib/mistos/zx/debuglog.h>
#include <sys/types.h>

#include <cstdint>
#include <numeric>
#include <vector>

#include <fbl/string.h>
#include <ktl/byte.h>
#include <ktl/span.h>

#include "util.h"

namespace {

#define INTERP_PREFIX "lib/"
#define MAX_ARG_STRLEN (PAGE_SIZE * 32)

constexpr uint32_t AT_NULL = 0;
constexpr uint32_t AT_RANDOM = 25;
constexpr uint32_t AT_EXECFN = 31;

constexpr size_t kMaxSegments = 4;
constexpr size_t kMaxPhdrs = 16;

zx_vaddr_t load(const zx::debuglog& log, std::string_view what, const zx::vmar& vmar,
                const zx::vmo& vmo, uintptr_t* interp_off, size_t* interp_len,
                zx::vmar* segments_vmar, size_t* stack_size, bool return_entry, LoadedElf* info) {
  auto diag = elfldltl::Diagnostics(elfldltl::PrintfDiagnosticsReport(
                                        [&log](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
                                          printl(log, args...);
#pragma GCC diagnostic pop
                                        },
                                        "mistos-userboot: ", what, ": "),
                                    elfldltl::DiagnosticsPanicFlags());

  elfldltl::UnownedVmoFile file(vmo.borrow(), diag);
  auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
      diag, file, elfldltl::FixedArrayFromFile<elfldltl::Elf<>::Phdr, kMaxPhdrs>());
  ZX_ASSERT(headers);
  auto& [ehdr, phdrs_result] = *headers;
  cpp20::span<const elfldltl::Elf<>::Phdr> phdrs = phdrs_result;

  std::optional<size_t> stack;
  std::optional<elfldltl::Elf<>::Phdr> interp;
  elfldltl::StaticOrDynExecutableVmarLoader loader{vmar, (ehdr.type == elfldltl::ElfType::kDyn)};
  elfldltl::LoadInfo<elfldltl::Elf<>, elfldltl::StaticVector<kMaxSegments>::Container> load_info;
  bool executable;
  ZX_ASSERT(
      elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader.page_size()),
                            elfldltl::PhdrInterpObserver<elfldltl::Elf<>>(interp),
                            elfldltl::PhdrStackObserver<elfldltl::Elf<>, true>(stack, executable)));
  if (executable) {
    printl(log, "executable stack.\n");
  } else {
    printl(log, "non-executable stack.\n");
  }

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

  if (info) {
    *info = LoadedElf{ehdr, zx::vmo(), base, loader.load_bias()};
  }

  printl(log, "loaded %.*s at %p, entry point %p\n", static_cast<int>(what.size()),
         what.data(), (void*)base, (void*)entry);
  return return_entry ? entry : base;
}

}  // namespace

zx_vaddr_t elf_load_bootfs(const zx::debuglog& log, zbi_parser::Bootfs& bootfs,
                           std::string_view root, const zx::vmar& vmar, std::string_view filename,
                           size_t* stack_size, ElfInfo* info) {
  uintptr_t interp_off = 0;
  size_t interp_len = 0;
  auto result = bootfs.Open(root, filename, "program");
  if (result.is_error()) {
    return result.error_value();
  }

  zx::vmo vmo = std::move(result.value());
  if (!vmo.is_valid()) {
    return ZX_ERR_INVALID_ARGS;
  }

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
    check(log, status, "zx_vmo_read failed: %d", status);

    // Copy the nul.
    interp[kInterpPrefixLen + interp_len] = '\0';

    printl(log, "'%.*s' has PT_INTERP \"%s\"", static_cast<int>(filename.size()), filename.data(),
           interp);

    auto result2 = bootfs.Open(root, interp, "dynamic linker");
    if (result2.is_error()) {
      return result2.error_value();
    }
    zx::vmo interp_vmo = std::move(result2.value());
    zx::vmar interp_vmar;
    entry = load(log, interp, vmar, interp_vmo, NULL, NULL, &interp_vmar, NULL, true,
                 (info) ? &info->interp_elf : nullptr);
    if (info) {
      info->has_interp = true;
      info->interp_elf.vmo = std::move(interp_vmo);
    }
  }

  if (info) {
    info->main_elf.vmo = std::move(vmo);
  }

  return entry;
}

size_t get_initial_stack_size(const fbl::String& path, const std::vector<fbl::String>& argv,
                              const std::vector<fbl::String>& environ,
                              const std::vector<std::pair<uint32_t, uint64_t>>& auxv) {
  auto accumulate_size = [](size_t accumulator, const auto& arg) {
    return accumulator + arg.length() + 1;
  };

  size_t stack_size = std::accumulate(argv.begin(), argv.end(), 0, accumulate_size);
  stack_size += std::accumulate(environ.begin(), environ.end(), 0, accumulate_size);
  stack_size += path.length() + 1;
  stack_size += kRandomSeedBytes;
  stack_size += ((argv.size() + 1) + (environ.size() + 1)) * sizeof(const char*);
  stack_size += auxv.size() * 2 * sizeof(uint64_t);
  return stack_size;
}

fit::result<zx_status_t, StackResult> populate_initial_stack(
    const zx::debuglog& log, zx::vmo& stack_vmo, const fbl::String& path,
    const std::vector<fbl::String>& argv, const std::vector<fbl::String>& envp,
    std::vector<std::pair<uint32_t, uint64_t>>& auxv, zx_vaddr_t mapping_base,
    zx_vaddr_t original_stack_start_addr) {
  zx_vaddr_t stack_pointer = original_stack_start_addr;
  auto write_stack = [&](ktl::span<const ktl::byte> data,
                         zx_vaddr_t addr) -> fit::result<zx_status_t> {
    printl(log, "write [%lx] - %p - %zu", addr, data.data(), data.size());
    auto vmo_offset = addr - mapping_base;
    auto ret = stack_vmo.write(data.data(), vmo_offset, data.size());
    if (ret != ZX_OK) {
      return fit::error{ret};
    }
    return fit::ok();
  };

  zx_vaddr_t argv_end = stack_pointer;
  for (auto iter = argv.rbegin(); iter != argv.rend(); ++iter) {
    ktl::span<const ktl::byte> arg{reinterpret_cast<const ktl::byte*>(iter->data()),
                                   iter->length() + 1};

    stack_pointer -= arg.size();
    auto result = write_stack(arg, stack_pointer);
    if (result.is_error())
      return result.take_error();
  }
  zx_vaddr_t argv_start = stack_pointer;

  zx_vaddr_t environ_end = stack_pointer;
  for (auto iter = envp.rbegin(); iter != envp.rend(); ++iter) {
    ktl::span<const ktl::byte> env{reinterpret_cast<const ktl::byte*>(iter->data()),
                                   iter->length() + 1};
    stack_pointer -= env.size();
    auto result = write_stack(env, stack_pointer);
    if (result.is_error())
      return result.take_error();
  }
  zx_vaddr_t environ_start = stack_pointer;

  // Write the path used with execve.
  stack_pointer -= path.length() + 1;
  zx_vaddr_t execfn_addr = stack_pointer;
  auto result = write_stack({reinterpret_cast<const ktl::byte*>(path.data()), path.length() + 1},
                            execfn_addr);
  if (result.is_error())
    return result.take_error();

  ktl::array<ktl::byte, kRandomSeedBytes> random_seed{};
  zx_cprng_draw(random_seed.data(), random_seed.size());
  stack_pointer -= random_seed.size();
  zx_vaddr_t random_seed_addr = stack_pointer;
  result = write_stack({random_seed.data(), random_seed.size()}, random_seed_addr);
  if (result.is_error())
    return result.take_error();
  stack_pointer = random_seed_addr;

  auxv.emplace_back(AT_EXECFN, static_cast<uint64_t>(execfn_addr));
  auxv.emplace_back(AT_RANDOM, static_cast<uint64_t>(random_seed_addr));
  auxv.emplace_back(AT_NULL, static_cast<uint64_t>(0));

  // After the remainder (argc/argv/environ/auxv) is pushed, the stack pointer must be 16 byte
  // aligned. This is required by the ABI and assumed by the compiler to correctly align SSE
  // operations. But this can't be done after it's pushed, since it has to be right at the top of
  // the stack. So we collect it all, align the stack appropriately now that we know the size,
  // and push it all at once.
  std::vector<ktl::byte> main_data;
  // argc
  uint64_t argc = argv.size();
  std::copy_n(cpp20::bit_cast<ktl::array<ktl::byte, sizeof(argc)>>(argc).data(), sizeof(argc),
              std::back_inserter(main_data));

  // argv
  std::vector<const ktl::byte> kZero(8, ktl::byte{0});
  zx_vaddr_t next_arg_addr = argv_start;
  for (auto arg : argv) {
    std::copy_n(cpp20::bit_cast<ktl::array<ktl::byte, sizeof(next_arg_addr)>>(next_arg_addr).data(),
                sizeof(next_arg_addr), std::back_inserter(main_data));
    next_arg_addr += arg.length() + 1;
  }
  std::copy_n(kZero.data(), kZero.size(), std::back_inserter(main_data));
  // environ
  zx_vaddr_t next_env_addr = environ_start;
  for (auto env : envp) {
    std::copy_n(cpp20::bit_cast<ktl::array<ktl::byte, sizeof(next_env_addr)>>(next_env_addr).data(),
                sizeof(next_env_addr), std::back_inserter(main_data));
    next_env_addr += env.length() + 1;
  }
  std::copy_n(kZero.data(), kZero.size(), std::back_inserter(main_data));
  // auxv
  size_t auxv_start_offset = main_data.size();
  for (auto kv : auxv) {
    std::copy_n(
        cpp20::bit_cast<ktl::array<ktl::byte, sizeof(uint64_t)>>(static_cast<uint64_t>(kv.first))
            .data(),
        sizeof(uint64_t), std::back_inserter(main_data));
    std::copy_n(cpp20::bit_cast<ktl::array<ktl::byte, sizeof(kv.second)>>(kv.second).data(),
                sizeof(kv.second), std::back_inserter(main_data));
  }
  size_t auxv_end_offset = main_data.size();

  // Time to push.
  stack_pointer -= main_data.size();
  stack_pointer -= stack_pointer % 16;
  result = write_stack(main_data, stack_pointer);
  if (result.is_error())
    return result.take_error();

  zx_vaddr_t auxv_start = stack_pointer + auxv_start_offset;
  zx_vaddr_t auxv_end = stack_pointer + auxv_end_offset;

  return fit::ok(StackResult{
      stack_pointer,
      auxv_start,
      auxv_end,
      argv_start,
      argv_end,
      environ_start,
      environ_end,
  });
}
