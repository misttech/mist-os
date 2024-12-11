// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/console.h>
#include <lib/mistos/starnix/kernel/runner/container.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <zircon/assert.h>

#include <ktl/enforce.h>

namespace starnix {
namespace {

int starnix_main(int argc, const cmd_args* argv, uint32_t flags) {
  int rc = 0;
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments:\n");

  usage:
    printf("%s run <path to binary>\n", argv[0].str);
    return -1;
  }

  if (!strcmp(argv[1].str, "run")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    starnix_kernel_runner::Config config;
    int idx = 2;
    int remain = argc - idx;
    while (remain-- > 0) {
      fbl::AllocChecker ac;
      config.init.push_back(argv[idx++].str, &ac);
      ZX_ASSERT(ac.check());
    }
    config.name = "starnix-container";

    fbl::AllocChecker ac;
    config.mounts.push_back("/:bootfs:/:nosuid,nodev,relatime", &ac);
    ZX_ASSERT(ac.check());
    /*config.mounts.push_back("/dev:devtmpfs::nosuid,relatime", &ac);
    ZX_ASSERT(ac.check());
    config.mounts.push_back("/dev/pts:devpts::nosuid,noexec,relatime", &ac);
    ZX_ASSERT(ac.check());
    config.mounts.push_back("/dev/shm:tmpfs::nosuid,nodev", &ac);
    ZX_ASSERT(ac.check());
    config.mounts.push_back("/proc:proc::nosuid,nodev,noexec,relatime", &ac);
    ZX_ASSERT(ac.check());
    config.mounts.push_back("/sys:sysfs::nosuid,nodev,noexec,relatime", &ac);
    ZX_ASSERT(ac.check());*/
    config.mounts.push_back("/tmp:tmpfs", &ac);
    ZX_ASSERT(ac.check());

    auto container =
        starnix_uapi::make_source_context(starnix_kernel_runner::create_container(config))
            .with_source_context([&config]() {
              return mtl::format("creating container: \"%.*s\"",
                                 static_cast<int>(config.name.size()), config.name.data());
            });
    if (container.is_error()) {
      auto& error = container.error_value();
      error.print([](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
        printf(ktl::forward<decltype(args)>(args)...);
#pragma GCC diagnostic pop
        printf("\n");
      });

      return -1;
    }

  } else {
    printf("unrecognized subcommand\n");
    goto usage;
  }
  return rc;
}

}  // namespace
}  // namespace starnix

STATIC_COMMAND_START
STATIC_COMMAND("starnix", "Run elf executable in starnix runtime", &starnix::starnix_main)
STATIC_COMMAND_END(starnix)
