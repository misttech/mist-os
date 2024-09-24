// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/loader.h"

#include <lib/crypto/global_prng.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/math.h>
#include <lib/mistos/starnix_uapi/time.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/cprng.h>
#include <lib/starnix/elfldtl/vmo.h>
#include <trace.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <numeric>
#include <optional>

#include <explicit-memory/bytes.h>
#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/static_vector.h>
#include <fbl/vector.h>
#include <ktl/byte.h>
#include <ktl/numeric.h>
#include <ktl/span.h>
#include <ktl/string_view.h>

#include "../kernel_priv.h"
#include "starnix-loader.h"

#include <ktl/enforce.h>

#include <linux/auxvec.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace {

using namespace starnix;

constexpr size_t kMaxSegments = 4;
constexpr size_t kMaxPhdrs = 16;
const size_t kRandomSeedBytes = 16;

size_t get_initial_stack_size(const ktl::string_view& path,
                              const fbl::Vector<ktl::string_view>& argv,
                              const fbl::Vector<ktl::string_view>& environ,
                              const fbl::Vector<ktl::pair<uint32_t, uint64_t>>& auxv) {
  auto accumulate_size = [](size_t accumulator, const auto& arg) {
    return accumulator + arg.length() + 1;
  };

  size_t stack_size = ktl::accumulate(argv.begin(), argv.end(), 0, accumulate_size);
  stack_size += ktl::accumulate(environ.begin(), environ.end(), 0, accumulate_size);
  stack_size += path.length() + 1;
  stack_size += kRandomSeedBytes;
  stack_size += ((argv.size() + 1) + (environ.size() + 1)) * sizeof(const char*);
  stack_size += auxv.size() * 2 * sizeof(uint64_t);
  return stack_size;
}

constexpr size_t kMaxCPRNGDraw = ZX_CPRNG_DRAW_MAX_LEN;

zx_status_t cprng_draw(void* buffer, size_t len) {
  if (len > kMaxCPRNGDraw)
    return ZX_ERR_INVALID_ARGS;

  uint8_t kernel_buf[kMaxCPRNGDraw];
  // Ensure we get rid of the stack copy of the random data as this function returns.
  explicit_memory::ZeroDtor<uint8_t> zero_guard(kernel_buf, sizeof(kernel_buf));

  auto prng = crypto::global_prng::GetInstance();
  ASSERT(prng->is_thread_safe());
  prng->Draw(kernel_buf, len);

  memcpy(buffer, kernel_buf, len);
  return ZX_OK;
}

fit::result<Errno, StackResult> populate_initial_stack(
    const MemoryAccessor& ma, const ktl::string_view& path,
    const fbl::Vector<ktl::string_view>& argv, const fbl::Vector<ktl::string_view>& envp,
    fbl::Vector<ktl::pair<uint32_t, uint64_t>>& auxv, UserAddress original_stack_start_addr) {
  auto stack_pointer = original_stack_start_addr;

  auto write_stack = [&ma](const ktl::span<const uint8_t>& data,
                           UserAddress addr) -> fit::result<Errno, size_t> {
    LTRACEF("write [%lx] - %p - %zu\n", addr.ptr(), data.data(), data.size());
    return ma.write_memory(addr, data);
  };

  auto argv_end = stack_pointer;
  for (auto iter = argv.rbegin(); iter != argv.rend(); ++iter) {
    ktl::span<const uint8_t> arg{reinterpret_cast<const uint8_t*>(iter->data()),
                                 iter->length() + 1};

    stack_pointer -= arg.size();
    auto result = write_stack(arg, stack_pointer);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  auto argv_start = stack_pointer;

  auto environ_end = stack_pointer;
  for (auto iter = envp.rbegin(); iter != envp.rend(); ++iter) {
    ktl::span<const uint8_t> env{reinterpret_cast<const uint8_t*>(iter->data()),
                                 iter->length() + 1};
    stack_pointer -= env.size();
    auto result = write_stack(env, stack_pointer);
    if (result.is_error())
      return result.take_error();
  }
  auto environ_start = stack_pointer;

  // Write the path used with execve.
  stack_pointer -= path.length() + 1;
  auto execfn_addr = stack_pointer;
  auto result =
      write_stack({reinterpret_cast<const uint8_t*>(path.data()), path.length() + 1}, execfn_addr);
  if (result.is_error()) {
    return result.take_error();
  }

  ktl::array<uint8_t, kRandomSeedBytes> random_seed{};
  cprng_draw(random_seed.data(), random_seed.size());
  stack_pointer -= random_seed.size();
  auto random_seed_addr = stack_pointer;
  result = write_stack({random_seed.data(), random_seed.size()}, random_seed_addr);
  if (result.is_error()) {
    return result.take_error();
  }
  stack_pointer = random_seed_addr;

  fbl::AllocChecker ac;
  auxv.push_back(ktl::pair(AT_EXECFN, static_cast<uint64_t>(execfn_addr.ptr())), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_RANDOM, static_cast<uint64_t>(random_seed_addr.ptr())), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_NULL, static_cast<uint64_t>(0)), &ac);
  ZX_ASSERT(ac.check());

  // After the remainder (argc/argv/environ/auxv) is pushed, the stack pointer must be 16 byte
  // aligned. This is required by the ABI and assumed by the compiler to correctly align SSE
  // operations. But this can't be done after it's pushed, since it has to be right at the top of
  // the stack. So we collect it all, align the stack appropriately now that we know the size,
  // and push it all at once.
  fbl::Vector<uint8_t> main_data;
  // argc
  uint64_t argc = argv.size();
  ktl::span<uint8_t> argc_data(reinterpret_cast<uint8_t*>(&argc), sizeof(argc));
  ktl::copy_n(argc_data.data(), argc_data.size(), util::back_inserter(main_data));

  // argv
  constexpr fbl::static_vector<uint8_t, 8> kZero(8, 0u);
  auto next_arg_addr = argv_start;
  for (auto arg : argv) {
    ktl::span<uint8_t> ptr(reinterpret_cast<uint8_t*>(&next_arg_addr), sizeof(next_arg_addr));
    ktl::copy_n(ptr.data(), ptr.size(), util::back_inserter(main_data));
    next_arg_addr += arg.length() + 1;
  }
  ktl::copy(kZero.begin(), kZero.end(), util::back_inserter(main_data));
  // environ
  auto next_env_addr = environ_start;
  for (auto env : envp) {
    ktl::span<uint8_t> ptr(reinterpret_cast<uint8_t*>(&next_env_addr), sizeof(next_env_addr));
    ktl::copy_n(ptr.data(), ptr.size(), util::back_inserter(main_data));
    next_env_addr += env.length() + 1;
  }
  ktl::copy(kZero.begin(), kZero.end(), util::back_inserter(main_data));
  // auxv
  size_t auxv_start_offset = main_data.size();
  for (auto kv : auxv) {
    uint64_t key = static_cast<uint64_t>(kv.first);
    ktl::span<uint8_t> key_span(reinterpret_cast<uint8_t*>(&key), sizeof(key));
    ktl::span<uint8_t> value_span(reinterpret_cast<uint8_t*>(&kv.second), sizeof(kv.second));

    ktl::copy_n(key_span.data(), key_span.size(), util::back_inserter(main_data));
    ktl::copy_n(value_span.data(), value_span.size(), util::back_inserter(main_data));
  }
  size_t auxv_end_offset = main_data.size();

  // Time to push.
  stack_pointer -= main_data.size();
  stack_pointer -= stack_pointer.ptr() % 16;
  result = write_stack(main_data, stack_pointer);
  if (result.is_error())
    return result.take_error();

  auto auxv_start = stack_pointer + auxv_start_offset;
  auto auxv_end = stack_pointer + auxv_end_offset;

  return fit::ok(StackResult{
      .stack_pointer = stack_pointer,
      .auxv_start = auxv_start,
      .auxv_end = auxv_end,
      .argv_start = argv_start,
      .argv_end = argv_end,
      .environ_start = environ_start,
      .environ_end = environ_end,
  });
}

auto GetDiagnostics() {
  return elfldltl::Diagnostics(elfldltl::PrintfDiagnosticsReport(
                                   [](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
                                     if (LOCAL_TRACE)
                                       printf(args...);
#pragma GCC diagnostic pop
                                   },
                                   "resolve_elf: "),
                               elfldltl::DiagnosticsPanicFlags());
}

struct LoadedElf {
  elfldltl::Elf<>::Ehdr file_header;
  size_t file_base;
  size_t vaddr_bias;
};

fit::result<Errno, LoadedElf> load_elf(
    const FileHandle& file, fbl::RefPtr<MemoryObject> elf_memory,
    const fbl::RefPtr<starnix::MemoryManager>& mm /*file_write_guard: FileWriteGuardRef,*/) {
  auto vmo = elf_memory->as_vmo();
  if (!vmo) {
    return fit::error(errno(EINVAL));
  }

  auto diag = GetDiagnostics();
  elfldltl::VmoFile vmo_file(vmo.value().get().dispatcher()->vmo(), diag);
  auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
      diag, vmo_file, elfldltl::FixedArrayFromFile<elfldltl::Elf<>::Phdr, kMaxPhdrs>());
  ZX_ASSERT(headers);
  auto& [ehdr, phdrs_result] = *headers;
  ktl::span<const elfldltl::Elf<>::Phdr> phdrs = phdrs_result;

  elfldltl::LoadInfo<elfldltl::Elf<>, elfldltl::StaticVector<kMaxSegments>::Container> load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(PAGE_SIZE)));

  size_t file_base = 0;
  if (ehdr.type == elfldltl::ElfType::kDyn) {
    file_base = mm->get_random_base(load_info.vaddr_size()).ptr();
  } else if (ehdr.type == elfldltl::ElfType::kExec) {
    file_base = load_info.vaddr_start();
  } else {
    return fit::error(errno(EINVAL));
  }
  size_t vaddr_bias = file_base - load_info.vaddr_start();

  StarnixLoader loader(mm);
  ZX_ASSERT(loader.map_elf_segments(diag, load_info, elf_memory, mm->base_addr.ptr(), vaddr_bias));

  LTRACEF("loaded at %lx, entry point %lx\n", file_base, ehdr.entry + vaddr_bias);

  return fit::ok(LoadedElf{.file_header = ehdr, .file_base = file_base, .vaddr_bias = vaddr_bias});
}

ktl::optional<ktl::string_view> from_bytes_until_nul(const char* bytes, size_t len) {
  const char* nul_pos = static_cast<const char*>(memchr(bytes, '\0', len));
  if (nul_pos == nullptr) {
    return ktl::nullopt;
  }
  return ktl::string_view(bytes, nul_pos - bytes);
}

// Resolves a file handle into a validated executable ELF.
fit::result<Errno, starnix::ResolvedElf> resolve_elf(
    const CurrentTask& current_task, const starnix::FileHandle& file,
    fbl::RefPtr<MemoryObject> memory, const fbl::Vector<ktl::string_view>& argv,
    const fbl::Vector<ktl::string_view>& environ
    /*,selinux_state: Option<SeLinuxResolvedElfState>*/) {
  auto vmo = memory->as_vmo();
  if (!vmo) {
    return fit::error(errno(EINVAL));
  }

  ktl::optional<starnix::ResolvedInterpElf> resolved_interp;

  auto diag = GetDiagnostics();
  elfldltl::VmoFile vmo_file(vmo.value().get().dispatcher()->vmo(), diag);
  auto elf_headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
      diag, vmo_file, elfldltl::FixedArrayFromFile<elfldltl::Elf<>::Phdr, kMaxPhdrs>());
  ZX_ASSERT(elf_headers);
  auto& [ehdr, phdrs_result] = *elf_headers;
  ktl::span<const elfldltl::Elf<>::Phdr> phdrs = phdrs_result;

  ktl::optional<elfldltl::Elf<>::Phdr> interp;
  elfldltl::LoadInfo<elfldltl::Elf<>, elfldltl::StaticVector<kMaxSegments>::Container> load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(PAGE_SIZE),
                                  elfldltl::PhdrInterpObserver<elfldltl::Elf<>>(interp)));
  if (interp) {
    // The ELF header specified an ELF interpreter.
    // Read the path and load this ELF as well.

    auto interp_data = memory->read_to_vec(interp->offset, interp->filesz);
    if (interp_data.is_error()) {
      return fit::error(errno(from_status_like_fdio(interp_data.error_value())));
    }

    auto interp_path =
        from_bytes_until_nul(reinterpret_cast<char*>(interp_data->data()), interp_data->size());
    if (!interp_path.has_value()) {
      return fit::error(errno(EINVAL));
    }
    LTRACEF("PT_INTERP=[%.*s]\n", static_cast<int>(interp_path->size()), interp_path->data());

    auto interp_file = current_task.open_file(*interp_path, OpenFlags(OpenFlagsEnum::RDONLY));
    if (interp_file.is_error()) {
      return interp_file.take_error();
    }

    auto interp_memory = interp_file->get_memory(
        current_task, {},
        ProtectionFlags(ProtectionFlagsEnum::READ) | ProtectionFlags(ProtectionFlagsEnum::EXEC));
    if (interp_memory.is_error()) {
      return interp_memory.take_error();
    }
    /*
      let file_write_guard =
            interp_file.name.entry.node.create_write_guard(FileWriteGuardMode::Exec)?.into_ref();
    */

    resolved_interp =
        starnix::ResolvedInterpElf{.file = interp_file.value(), .memory = interp_memory.value()};
  }

  /*
  let file_write_guard =
      file.name.entry.node.create_write_guard(FileWriteGuardMode::Exec)?.into_ref();
  */

  fbl::Vector<ktl::string_view> argv_cpy;
  ktl::copy(argv.begin(), argv.end(), util::back_inserter(argv_cpy));

  fbl::Vector<ktl::string_view> environ_cpy;
  ktl::copy(environ.begin(), environ.end(), util::back_inserter(environ_cpy));

  return fit::ok(starnix::ResolvedElf{
      .file = file,
      .memory = memory,
      .interp = ktl::move(resolved_interp),
      .argv = ktl::move(argv_cpy),
      .environ = ktl::move(environ_cpy) /*, selinux_state, file_write_guard*/});
}

// Resolves a #! script file into a validated executable ELF.
fit::result<Errno, starnix::ResolvedElf> resolve_script(
    const starnix::CurrentTask& current_task, fbl::RefPtr<MemoryObject> memory,
    const ktl::string_view& path, const fbl::Vector<ktl::string_view>& argv,
    const fbl::Vector<ktl::string_view>& environ, size_t recursion_depth
    /*,selinux_state: Option<SeLinuxResolvedElfState>*/) {
  return fit::error(errno(-1));
}

// Resolves a file into a validated executable ELF, following script interpreters to a fixed
// recursion depth.
fit::result<Errno, starnix::ResolvedElf> resolve_executable_impl(
    const starnix::CurrentTask& current_task, const starnix::FileHandle& file,
    ktl::string_view path, const fbl::Vector<ktl::string_view>& argv,
    const fbl::Vector<ktl::string_view>& environ, size_t recursion_depth
    /*,selinux_state: Option<SeLinuxResolvedElfState>*/) {
  if (recursion_depth > MAX_RECURSION_DEPTH) {
    return fit::error(errno(ELOOP));
  }

  auto memory = file->get_memory(
      current_task, {},
      ProtectionFlags(ProtectionFlagsEnum::READ) | ProtectionFlags(ProtectionFlagsEnum::EXEC));
  if (memory.is_error()) {
    return memory.take_error();
  }

  auto header = memory->read_to_array<char, HASH_BANG_SIZE>(0);
  if (header.is_error()) {
    switch (header.error_value()) {
      case ZX_ERR_OUT_OF_RANGE:
        return fit::error(errno(ENOEXEC));
      default:
        return fit::error(errno(EINVAL));
    }
  }

  if (header.value() == HASH_BANG) {
    return resolve_script(
        current_task, ktl::move(memory.value()), path, argv, environ, recursion_depth
        /*, selinux_state*/);
  } else {
    return resolve_elf(current_task, file, ktl::move(memory.value()), argv,
                       environ /*, selinux_state*/);
  }
}

}  // namespace

namespace starnix {

fit::result<Errno, ResolvedElf> resolve_executable(
    const CurrentTask& current_task, const FileHandle& file, const ktl::string_view& path,
    const fbl::Vector<ktl::string_view>& argv,
    const fbl::Vector<ktl::string_view>&
        environ /*,selinux_state: Option<SeLinuxResolvedElfState>*/) {
  return resolve_executable_impl(current_task, file, path, argv, environ, 0);
}

fit::result<Errno, ThreadStartInfo> load_executable(const CurrentTask& current_task,
                                                    const ResolvedElf& resolved_elf,
                                                    const ktl::string_view& original_path) {
  auto main_elf = load_elf(resolved_elf.file, resolved_elf.memory, current_task->mm()/*,
                           resolved_elf.file_write_guard*/);
  if (main_elf.is_error()) {
    return main_elf.take_error();
  }

  ktl::optional<LoadedElf> interp_elf;
  if (resolved_elf.interp.has_value()) {
    auto& interp = resolved_elf.interp.value();
    auto load_interp_result = load_elf(interp.file, interp.memory, current_task->mm()/*,
                           resolved_elf.file_write_guard*/);
    if (load_interp_result.is_error()) {
      return load_interp_result.take_error();
    }
    interp_elf = load_interp_result.value();
  }

  auto entry_elf = interp_elf.value_or(main_elf.value());
  auto entry = entry_elf.file_header.entry + entry_elf.vaddr_bias;

  LTRACEF("loaded %.*s at entry point 0x%lx\n", static_cast<int>(original_path.size()),
          original_path.data(), entry);
  /*
    let vdso_vmo = &current_task.kernel().vdso.vmo;
    let vvar_vmo = current_task.kernel().vdso.vvar_readonly.clone();

    let vdso_size = vdso_vmo.get_size().map_err(|_| errno!(EINVAL))?;
    const VDSO_PROT_FLAGS: ProtectionFlags = ProtectionFlags::READ.union(ProtectionFlags::EXEC);

    let vvar_size = vvar_vmo.get_size().map_err(|_| errno!(EINVAL))?;
    const VVAR_PROT_FLAGS: ProtectionFlags = ProtectionFlags::READ;

    // Create a private clone of the starnix kernel vDSO
    let vdso_clone = vdso_vmo
        .create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, vdso_size)
        .map_err(|status| from_status_like_fdio!(status))?;

    let vdso_executable = vdso_clone
        .replace_as_executable(&VMEX_RESOURCE)
        .map_err(|status| from_status_like_fdio!(status))?;

    // Memory map the vvar vmo, mapping a space the size of (size of vvar + size of vDSO)
    let vvar_map_result = current_task.mm().map_vmo(
        DesiredAddress::Any,
        vvar_vmo,
        0,
        (vvar_size as usize) + (vdso_size as usize),
        VVAR_PROT_FLAGS,
        MappingOptions::empty(),
        MappingName::Vvar,
        FileWriteGuardRef(None),
    )?;

    // Overwrite the second part of the vvar mapping to contain the vDSO clone
    let vdso_base = current_task.mm().map_vmo(
        DesiredAddress::FixedOverwrite(vvar_map_result + vvar_size),
        Arc::new(vdso_executable),
        0,
        vdso_size as usize,
        VDSO_PROT_FLAGS,
        MappingOptions::DONT_SPLIT,
        MappingName::Vdso,
        FileWriteGuardRef(None),
    )?;
  */
  auto vdso_base = UserAddress();

  auto creds = current_task->creds();
  auto secure = [&creds]() {
    if (creds.uid != creds.euid || creds.gid != creds.egid) {
      return 1;
    } else {
      return 0;
    }
  }();

  fbl::AllocChecker ac;
  fbl::Vector<ktl::pair<uint32_t, uint64_t>> auxv;
  auxv.push_back(ktl::pair(AT_UID, creds.uid), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_EUID, creds.euid), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_GID, creds.gid), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_EGID, creds.egid), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_BASE, interp_elf.value_or(LoadedElf{.file_base = 0}).file_base), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PAGESZ, static_cast<uint64_t>(PAGE_SIZE)), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHDR, main_elf->file_base + main_elf->file_header.phoff), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHENT, main_elf->file_header.phentsize), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHNUM, main_elf->file_header.phnum), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_ENTRY, main_elf->vaddr_bias + main_elf->file_header.entry), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_CLKTCK, SCHEDULER_CLOCK_HZ), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_SYSINFO_EHDR, vdso_base.ptr()), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_SECURE, secure), &ac);
  ZX_ASSERT(ac.check());

  // TODO(tbodt): implement MAP_GROWSDOWN and then reset this to 1 page. The current value of
  // this is based on adding 0x1000 each time a segfault appears.
  auto stack_size_result = round_up_to_system_page_size(
      get_initial_stack_size(original_path, resolved_elf.argv, resolved_elf.environ, auxv) +
      0xf0000);
  if (stack_size_result.is_error()) {
    LTRACEF("Stack is too big\n");
    return stack_size_result.take_error();
  }

  auto prot_flags =
      ProtectionFlags(ProtectionFlagsEnum::READ) | ProtectionFlags(ProtectionFlagsEnum::WRITE);

  auto stack_base = current_task->mm()->map_anonymous(
      {.type = DesiredAddressType::Any, .address = 0}, stack_size_result.value(), prot_flags,
      MappingOptionsFlags(MappingOptions::ANONYMOUS), {.type = MappingNameType::Stack});
  if (stack_base.is_error()) {
    TRACEF("Failed map stack\n");
    return stack_base.take_error();
  }

  auto stack = stack_base.value() + (stack_size_result.value() - 8);

  LTRACEF("stack [%lx, %lx) sp=%lx\n", stack_base.value().ptr(),
          stack_base.value().ptr() + stack_size_result.value(), stack.ptr());

  auto stack_result = populate_initial_stack(current_task, original_path, resolved_elf.argv,
                                             resolved_elf.environ, auxv, stack);
  if (stack_result.is_error()) {
    TRACEF("Failed to populate initial stack\n");
    return stack_result.take_error();
  }

  auto mm_state = current_task->mm()->state.Write();
  (*mm_state)->stack_base = stack_base.value();
  (*mm_state)->stack_size = stack_size_result.value();
  (*mm_state)->stack_start = stack_result->stack_pointer;
  (*mm_state)->auxv_start = stack_result->auxv_start;
  (*mm_state)->auxv_end = stack_result->auxv_end;
  (*mm_state)->argv_start = stack_result->argv_start;
  (*mm_state)->argv_end = stack_result->argv_end;
  (*mm_state)->environ_start = stack_result->environ_start;
  (*mm_state)->environ_end = stack_result->environ_end;

  (*mm_state)->vdso_base = vdso_base;

  return fit::ok(ThreadStartInfo{.entry = entry, .stack = stack_result->stack_pointer});
}

fit::result<Errno, StackResult> test_populate_initial_stack(
    const MemoryAccessor& ma, const ktl::string_view& path,
    const fbl::Vector<ktl::string_view>& argv, const fbl::Vector<ktl::string_view>& envp,
    fbl::Vector<ktl::pair<uint32_t, uint64_t>>& auxv, UserAddress original_stack_start_addr) {
  return populate_initial_stack(ma, path, argv, envp, auxv, original_stack_start_addr);
}

}  // namespace starnix
