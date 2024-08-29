// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/runner/container.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <trace.h>
#include <zircon/assert.h>

#include <vector>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <ktl/algorithm.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fit::result<Errno, Container> create_container(const ConfigWrapper& config) {
  const fbl::String DEFAULT_INIT("/container/init");

  fbl::RefPtr<Kernel> kernel = Kernel::New(config->kernel_cmdline).value_or(fbl::RefPtr<Kernel>());
  ASSERT_MSG(kernel, "creating Kernel: %s\n", config->name.data());

  fbl::RefPtr<FsContext> fs_context;
  if (auto result = create_fs_context(kernel, config); result.is_error()) {
    LTRACEF("creating FsContext: %d\n", result.error_value());
    return fit::error(errno(from_status_like_fdio(result.error_value())));
  } else {
    fs_context = result.value();
  }

  auto init_pid = kernel->pids.Write()->allocate_pid();
  ASSERT(init_pid == 1);

  /*
  mount_filesystems(locked, &system_task, config, &pkg_dir_proxy)
        .source_context("mounting filesystems")?;
  */

  // If there is an init binary path, run it, optionally waiting for the
  // startup_file_path to be created. The task struct is still used
  // to initialize the system up until this point, regardless of whether
  // or not there is an actual init to be run.
  auto argv = [&]() -> fbl::Vector<fbl::String> {
    fbl::Vector<fbl::String> argv;
    if (config->init.is_empty()) {
      fbl::AllocChecker ac;
      argv.push_back(DEFAULT_INIT, &ac);
      ZX_ASSERT(ac.check());
    } else {
      ktl::copy(config->init.begin(), config->init.end(), util::back_inserter(argv));
    }
    return ktl::move(argv);
  }();

  auto executable = CurrentTask::open_file_bootfs(argv[0] /*, OpenFlags::RDONLY*/);
  if (executable.is_error())
    return executable.take_error();

  auto initial_name = fbl::String();
  if (!config->init.is_empty()) {
    initial_name = config->init[0];
  };

  auto init_task = CurrentTask::create_init_process(kernel, init_pid, initial_name, fs_context);
  if (init_task.is_error())
    return init_task.take_error();

  if (LOCAL_TRACE) {
    printf("creating init task: ");
    for (auto arg : config->init) {
      printf("%s ", arg.data());
    }
    printf("\n");
  }

  auto pre_run = [&](CurrentTask& init_task) -> fit::result<Errno> {
    return init_task.exec(executable.value(), argv[0], ktl::move(argv), fbl::Vector<fbl::String>());
  };

  auto task_complete = []() -> void { LTRACEF("Finished running init process.\n"); };

  execute_task_with_prerun_result(init_task.value(), pre_run, task_complete);

  return fit::ok(Container{kernel});
}

fit::result<zx_status_t, fbl::RefPtr<FsContext>> create_fs_context(
    const fbl::RefPtr<Kernel>& kernel, const ConfigWrapper& config) {
  return fit::ok(ktl::move(FsContext::New(TmpFs::new_fs(kernel))));
}

}  // namespace starnix
