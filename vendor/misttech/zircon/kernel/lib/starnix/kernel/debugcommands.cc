// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/console.h>
#include <lib/mistos/starnix/kernel/runner/container.h>
#include <zircon/assert.h>

#include <ktl/enforce.h>

static int starnix_main(int argc, const cmd_args* argv, uint32_t flags);

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("starnix", "", &starnix_main, CMD_AVAIL_NORMAL)
STATIC_COMMAND_END(fs)

static int starnix_main(int argc, const cmd_args* argv, uint32_t flags) {
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

    starnix::Config config;
    int idx = 2;
    int remain = argc - idx;
    while (remain-- > 0) {
      fbl::AllocChecker ac;
      config.init.push_back(argv[idx++].str, &ac);
      ZX_ASSERT(ac.check());
    }
    config.name = "starnix-container";
    auto container = starnix::create_container(config);
    if (container.is_error()) {
      return container.error_value().error_code();
    }

  } else {
    printf("unrecognized subcommand\n");
    goto usage;
  }
  return rc;
}
