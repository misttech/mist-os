// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/console.h>
#include <lib/mistos/userloader/elf.h>
#include <lib/mistos/userloader/start.h>
#include <lib/mistos/userloader/userloader.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zbi_parser/zbi.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/zircon-internal/default_stack_size.h>

#include <fbl/alloc_checker.h>

#include "util.h"

#include <ktl/enforce.h>

static int elf_main(int argc, const cmd_args* argv, uint32_t flags);

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("elf", "load and start elf binary from ZBI", &elf_main, CMD_AVAIL_NORMAL)
STATIC_COMMAND_END(fs)

static int elf_main(int argc, const cmd_args* argv, uint32_t flags) {
  int rc = 0;
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments:\n");

  usage:
    printf("%s start <path to binary>\n", argv[0].str);
    return -1;
  }

  zx::debuglog log;
  zx::debuglog::create(zx::resource(), 0, &log);

  if (!strcmp(argv[1].str, "start")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    ktl::array<zx_handle_t, userloader::kHandleCount> handles =
        ExtractHandles(userloader::gHandles);

    zx::unowned_vmar vmar_self = zx::vmar::root_self();

    auto [power, vmex] = CreateResources(log, handles);

    // Locate the ZBI_TYPE_STORAGE_BOOTFS item and decompress it. This will be used to load
    // the binary referenced by userboot.next, as well as libc. Bootfs will be fully parsed
    // and hosted under '/boot' either by bootsvc or component manager.
    const zx::unowned_vmo zbi{handles[userloader::kZbi]};

    auto result = zbi_parser::GetBootfsFromZbi(*vmar_self, *zbi, true);
    if (result.is_error()) {
      printl(log, "failed to load bootfs from zbi");
      return -1;
    }

    zx::vmo bootfs_vmo = ktl::move(result.value());
    if (!bootfs_vmo.is_valid()) {
      printl(log, "failed to load from zbi");
      return -1;
    }

    zbi_parser::Bootfs bootfs{vmar_self->borrow(), ktl::move(bootfs_vmo), ktl::move(vmex)};

    ChildContext child = CreateChildContext(log, argv[2].str);

    zbi_parser::Options::ProgramInfo elf_entry{"", argv[2].str};

    fbl::AllocChecker ac;
    fbl::Vector<fbl::String> envp;
    envp.push_back("HOME=/", &ac);
    ZX_ASSERT(ac.check());
    envp.push_back("TERM=linux", &ac);
    ZX_ASSERT(ac.check());

    fbl::Vector<fbl::String> argv_vector;
    int idx = 2;
    int remain = argc - idx;
    while (remain-- > 0) {
      argv_vector.push_back(argv[idx++].str, &ac);
      ZX_ASSERT(ac.check());
    }

    if (StartChildProcess(log, elf_entry, child, bootfs, argv_vector, envp) == ZX_OK) {
      printl(log, "process %.*s started.", static_cast<int>(elf_entry.filename().size()),
             elf_entry.filename().data());
      WaitForProcessExit(log, elf_entry, child);
    }
  } else {
    printf("unrecognized subcommand\n");
    goto usage;
  }
  return rc;
}
