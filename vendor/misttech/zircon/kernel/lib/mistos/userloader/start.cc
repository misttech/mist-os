// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/elfldltl/machine.h>
#include <lib/mistos/userloader/elf.h>
#include <lib/mistos/userloader/start.h>
#include <lib/mistos/userloader/userloader.h>
#include <lib/mistos/util/system.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zbi_parser/zbi.h>
#include <lib/mistos/zx/job.h>
#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/resource.h>
#include <lib/mistos/zx/thread.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/zircon-internal/default_stack_size.h>
#include <zircon/assert.h>
#include <zircon/syscalls/resource.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <lk/init.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <platform/halt_helper.h>

#include "util.h"

#include <ktl/enforce.h>

// clang-format off
#include <linux/auxvec.h>
// clang-format on

constexpr const char kStackVmoName[] = "userboot-initial-stack";

using namespace userloader;

void ParseNextProcessArguments(const zx::debuglog& log, ktl::string_view next, uint32_t& argc,
                               char* argv) {
  // Extra byte for null terminator.
  size_t required_size = next.size() + 1;
  if (required_size > kProcessArgsMaxBytes) {
    fail(log, "required %zu bytes for process arguments, but only %u are available", required_size,
         kProcessArgsMaxBytes);
  }

  // At a minimum, child processes will be passed a single argument containing the binary name.
  argc++;
  uint32_t index = 0;
  for (char c : next) {
    if (c == '+') {
      // Argument list is provided as '+' separated, but passed as null separated. Every time
      // we encounter a '+' we replace it with a null and increment the argument counter.
      argv[index] = '\0';
      argc++;
    } else {
      argv[index] = c;
    }
    index++;
  }

  argv[index] = '\0';
}

static zx_status_t unpack_strings(char* buffer, uint32_t num, fbl::Vector<fbl::String>& result) {
  char* p = buffer;
  fbl::AllocChecker ac;
  for (uint32_t i = 0; i < num; ++i) {
    result.push_back(p, &ac);
    ZX_ASSERT(ac.check());
    while (*p++ != '\0')
      ;
  }
  return ZX_OK;
}

static void MapHandleToValue(Handle* handle, uint32_t* out) {
  ProcessDispatcher* dispatcher = nullptr;
  if (handle) {
    *out = handle_table(dispatcher).MapHandleToValue(handle);
  } else {
    *out = ZX_HANDLE_INVALID;
  }
}

ktl::array<zx_handle_t, kHandleCount> ExtractHandles(
    const ktl::array<Handle*, kHandleCount> handles) {
  ktl::array<zx_handle_t, kHandleCount> _handles = {};
  for (size_t i = 0; i < handles.size(); ++i) {
    MapHandleToValue(handles[i], &_handles[i]);
  }
  return _handles;
}

ChildContext CreateChildContext(const zx::debuglog& log, ktl::string_view name) {
  ChildContext child;
  auto status =
      zx::process::create(*zx::unowned_job{zx::job::default_job()}, name.data(),
                          static_cast<uint32_t>(name.size()), 0, &child.process, &child.vmar);
  CHECK(log, status, "Failed to create child process(%.*s).", static_cast<int>(name.length()),
        name.data());

  // Create the initial thread in the new process
  status = zx::thread::create(child.process, name.data(), static_cast<uint32_t>(name.size()), 0,
                              &child.thread);
  CHECK(log, status, "Failed to create main thread for child process(%.*s).",
        static_cast<int>(name.length()), name.data());

  return child;
}

Resources CreateResources(const zx::debuglog& log,
                          ktl::span<const zx_handle_t, kHandleCount> handles) {
  Resources resources = {};
  zx::unowned_resource system(handles[kSystemResource]);
  auto status = zx::resource::create(*system, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_POWER_BASE, 1,
                                     nullptr, 0, &resources.power);
  CHECK(log, status, "Failed to created power resource.");

  status = zx::resource::create(*system, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_VMEX_BASE, 1, nullptr,
                                0, &resources.vmex);
  CHECK(log, status, "Failed to created vmex resource.");
  return resources;
}

zx_status_t StartChildProcess(const zx::debuglog& log,
                              const zbi_parser::Options::ProgramInfo& elf_entry,
                              ChildContext& child, zbi_parser::Bootfs& bootfs,
                              const fbl::Vector<fbl::String>& argv,
                              const fbl::Vector<fbl::String>& envp) {
  size_t stack_size = ZIRCON_DEFAULT_STACK_SIZE;
  ElfInfo info;
  zx_status_t status;

  // Examine the bootfs image and find the requested file in it.
  // This will handle a PT_INTERP by doing a second lookup in bootfs.
  zx_vaddr_t entry = elf_load_bootfs(log, bootfs, elf_entry.root, child.vmar, elf_entry.filename(),
                                     &stack_size, &info);

  if (static_cast<zx_status_t>(entry) == ZX_ERR_INVALID_ARGS)
    return ZX_ERR_INVALID_ARGS;

  stack_size = (stack_size + zx_system_get_page_size() - 1) &
               -static_cast<uint64_t>(zx_system_get_page_size());

  zx::vmo stack_vmo;
  status = zx::vmo::create(stack_size, 0, &stack_vmo);
  if (status != ZX_OK) {
    printl(log, "zx_vmo_create failed for child stack");
    return status;
  }

  stack_vmo.set_property(ZX_PROP_NAME, kStackVmoName, sizeof(kStackVmoName) - 1);
  zx_vaddr_t stack_base;
  status =
      child.vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, stack_vmo, 0, stack_size, &stack_base);
  if (status != ZX_OK) {
    printl(log, "zx_vmar_map failed for child stack");
    return status;
  }

  // Allocate the stack for the child.
  uintptr_t sp = elfldltl::AbiTraits<>::InitialStackPointer(stack_base, stack_size);
  printl(log, "stack [%p, %p) sp=%p", reinterpret_cast<void*>(stack_base),
         reinterpret_cast<void*>(stack_base + stack_size), reinterpret_cast<void*>(sp));

  fbl::AllocChecker ac;
  fbl::Vector<ktl::pair<uint32_t, uint64_t>> auxv;
  auxv.push_back(ktl::pair(AT_PAGESZ, static_cast<uint64_t>(PAGE_SIZE)), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_BASE, info.has_interp ? info.interp_elf.base : 0), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHDR, info.main_elf.base + info.main_elf.header.phoff), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHENT, info.main_elf.header.phentsize), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_PHNUM, info.main_elf.header.phnum), &ac);
  ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_ENTRY, info.main_elf.load_bias + info.main_elf.header.entry), &ac);
  ZX_ASSERT(ac.check());
  // auxv.push_back(ktl::pair(AT_SYSINFO_EHDR, vdso_base));
  // ZX_ASSERT(ac.check());
  auxv.push_back(ktl::pair(AT_SECURE, 0), &ac);
  ZX_ASSERT(ac.check());

  auto result = populate_initial_stack(log, stack_vmo, elf_entry.filename(), argv, envp, auxv,
                                       stack_base, sp);

  if (result.is_error()) {
    printl(log, "failed to populate initial stack");
    return status;
  } else {
    auto stack_result = result.value();
    printl(log, "stack_result=%p", reinterpret_cast<void*>(stack_result.stack_pointer));

    // Start the process going.
    status = child.process.start(child.thread, entry, stack_result.stack_pointer, {}, 0);
    CHECK(log, status, "zx_process_start failed");
    child.thread.reset();
    return ZX_OK;
  }
}

int64_t WaitForProcessExit(const zx::debuglog& log, const zbi_parser::Options::ProgramInfo& entry,
                           const ChildContext& child) {
  printl(log, "Waiting for %.*s to exit...", static_cast<int>(entry.filename().size()),
         entry.filename().data());
  zx_status_t status = child.process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), nullptr);
  CHECK(log, status, "zx_object_wait_one on process failed");
  zx_info_process_t info;
  status = child.process.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr);
  CHECK(log, status, "zx_object_get_info on process failed");
  printl(log, "*** Exit status %zd ***\n", info.return_code);
  return info.return_code;
}

struct TerminationInfo {
  // Depending on test mode and result, this might be the return code of boot or test elf.
  ktl::optional<int64_t> test_return_code;

  // Whether we should continue or shutdown.
  bool should_shutdown = false;
};

[[noreturn]] void HandleTermination(const zx::debuglog& log, const TerminationInfo& info) {
  if (!info.should_shutdown) {
    printl(log, "finished!");
    ProcessDispatcher::ExitCurrent(0);
  }

  // The test runners match this exact string on the console log
  // to determine that the test succeeded since shutting the
  // machine down doesn't return a value to anyone for us.
  if (info.test_return_code && info.test_return_code == 0) {
    printl(log, "%s\n", BOOT_TEST_SUCCESS_STRING);
  }

  printl(log, "Process exited.  Executing poweroff");
  platform_graceful_halt_helper(HALT_ACTION_SHUTDOWN, ZirconCrashReason::NoCrash, ZX_TIME_INFINITE);
  printl(log, "still here after poweroff!");

  while (true)
    __builtin_trap();

  __UNREACHABLE;
}

// This is the main logic:
// 1. Load up the child process from ELF file(s) on the bootfs.
// 2. Create the initial thread and allocate a stack for it.
// 3. Start the child process running.
// 4. Optionally, wait for it to exit and then shut down.
void Bootstrap(uint) {
  // We pass all the same handles the kernel gives us along to the child,
  // except replacing our own process/root-VMAR handles with its, and
  // passing along the three extra handles (BOOTFS, thread-self, and a debuglog
  // handle tied to stdout).
  ktl::array<zx_handle_t, userloader::kHandleCount> handles = ExtractHandles(gHandles);

  zx::debuglog log;
  zx::debuglog::create(zx::resource(), 0, &log);

  zx::vmar vmar_self{handles[kVmarRootSelf]};
  handles[kVmarRootSelf] = ZX_HANDLE_INVALID;

  // zx::process proc_self{handles[kProcSelf]};
  // handles[kProcSelf] = ZX_HANDLE_INVALID;

  auto [power, vmex] = CreateResources(log, handles);

  // Locate the ZBI_TYPE_STORAGE_BOOTFS item and decompress it. This will be used to load
  // the binary referenced by userboot.next, as well as libc. Bootfs will be fully parsed
  // and hosted under '/boot' either by bootsvc or component manager.
  const zx::unowned_vmo zbi{handles[userloader::kZbi]};

  auto result = zbi_parser::GetBootfsFromZbi(vmar_self, *zbi, true);
  if (result.is_error()) {
    printl(log, "failed to load from zbi");
    return;
  }

  zx::vmo bootfs_vmo = ktl::move(result.value());
  if (!bootfs_vmo.is_valid()) {
    printl(log, "failed to load bootfs from zbi");
    return;
  }

  // Parse CMDLINE items to determine the set of runtime options.
  auto get_opts = zbi_parser::GetOptionsFromZbi(vmar_self, *zbi);
  if (get_opts.is_error()) {
    printl(log, "failed to load options from zbi");
    return;
  }
  zbi_parser::Options opts{get_opts.value()};

  TerminationInfo info;

  {
    // auto borrowed_bootfs = bootfs_vmo.borrow();
    zbi_parser::Bootfs bootfs{vmar_self.borrow(), ktl::move(bootfs_vmo), ktl::move(vmex)};
    auto launch_process = [&](auto& elf_entry) -> ChildContext {
      ChildContext child = CreateChildContext(log, elf_entry.filename());
      uint32_t argc = 0;
      fbl::AllocChecker ac;
      ktl::array<char, kProcessArgsMaxBytes> args;
      fbl::Vector<fbl::String> argv;
      fbl::Vector<fbl::String> envp;
      envp.push_back("HOME=/", &ac);
      ZX_ASSERT(ac.check());
      envp.push_back("TERM=linux", &ac);
      ZX_ASSERT(ac.check());

      // Fill in any '+' separated arguments provided by `userboot.next`. If arguments are longer
      // than kProcessArgsMaxBytes, this function will fail process creation.
      ParseNextProcessArguments(log, elf_entry.next, argc, args.data());
      unpack_strings(args.data(), argc, argv);

      if (StartChildProcess(log, elf_entry, child, bootfs, argv, envp) == ZX_OK) {
        printl(log, "process %.*s started.", static_cast<int>(elf_entry.filename().size()),
               elf_entry.filename().data());
      } else {
        ChildContext empty;
        return ktl::move(empty);
      }
      return child;
    };

    if (!opts.test.next.empty()) {
      // If no boot, then hand over the stash to the test program. Test does not get the svc stash.
      auto test_context = launch_process(opts.test);
      // Wait for test to finish.
      info.test_return_code = WaitForProcessExit(log, opts.test, test_context);

      info.should_shutdown = opts.boot.next.empty();
    }

    if (!opts.boot.next.empty()) {
      [[maybe_unused]] auto boot_context = launch_process(opts.boot);
    }
  }
  HandleTermination(log, info);
}

LK_INIT_HOOK(mistos_start, Bootstrap, LK_INIT_LEVEL_USER + 1)
