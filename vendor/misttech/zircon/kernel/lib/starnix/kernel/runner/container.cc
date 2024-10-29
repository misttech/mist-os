// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/runner/container.h"

#include <lib/fit/result.h>
#include <lib/handoff/handoff.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
#include <lib/mistos/starnix/kernel/fs/mistos/syslog.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/error_propagation.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <object/process_dispatcher.h>
#include <object/vm_object_dispatcher.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

Container::~Container() { LTRACE_ENTRY_OBJ; }

fit::result<Errno, Container> create_container(const Config& config) {
  const ktl::string_view DEFAULT_INIT("/container/init");

  fbl::RefPtr<Kernel> kernel = Kernel::New(config.kernel_cmdline).value_or(fbl::RefPtr<Kernel>());
  ASSERT_MSG(kernel, "creating Kernel: %.*s\n", static_cast<int>(config.name.size()),
             config.name.data());

  fbl::RefPtr<FsContext> fs_context;
  auto result = create_fs_context(kernel, config);
  if (result.is_error()) {
    LTRACEF("creating FsContext: %d\n", result.error_value());
    return fit::error(errno(from_status_like_fdio(result.error_value())));
  }
  fs_context = result.value();

  auto init_pid = kernel->pids.Write()->allocate_pid();
  // Lots of software assumes that the pid for the init process is 1.
  DEBUG_ASSERT(init_pid == 1);

  {
    auto system_task = CurrentTask::create_system_task(
                           /*kernel.kthreads.unlocked_for_async().deref_mut(),*/ kernel, fs_context)
                           .value();
    // The system task gives pid 2. This value is less critical than giving
    // pid 1 to init, but this value matches what is supposed to happen.
    DEBUG_ASSERT(system_task->id_ == 2);

    _EP(kernel->kthreads().Init(ktl::move(system_task)));
  }

  auto& system_task = kernel->kthreads().system_task();

  // Register common devices and add them in sysfs and devtmpfs.
  // init_common_devices(kernel.kthreads.unlocked_for_async().deref_mut(), &system_task);
  // register_common_file_systems(kernel.kthreads.unlocked_for_async().deref_mut(), &kernel);
  /*

  mount_filesystems(locked, &system_task, config, &pkg_dir_proxy)
        .source_context("mounting filesystems")?;
  */

  // If there is an init binary path, run it, optionally waiting for the
  // startup_file_path to be created. The task struct is still used
  // to initialize the system up until this point, regardless of whether
  // or not there is an actual init to be run.
  auto argv = [&]() -> fbl::Vector<BString> {
    fbl::Vector<BString> argv;
    if (config.init.is_empty()) {
      fbl::AllocChecker ac;
      argv.push_back(DEFAULT_INIT, &ac);
      ZX_ASSERT(ac.check());
    } else {
      ktl::copy(config.init.begin(), config.init.end(), util::back_inserter(argv));
    }
    return ktl::move(argv);
  }();

  auto executable = system_task.open_file(argv[0], OpenFlags(OpenFlagsEnum::RDONLY));
  if (executable.is_error())
    return executable.take_error();

  ktl::string_view initial_name;
  if (!config.init.is_empty()) {
    initial_name = config.init[0];
  }

  // let rlimits = parse_rlimits(&config.rlimits)?;
  fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>> rlimits;
  auto init_task = CurrentTask::create_init_process(kernel, init_pid, initial_name, fs_context,
                                                    ktl::move(rlimits)) _EP(init_task);

  if (LOCAL_TRACE) {
    TRACEF("creating init task: ");
    for (auto arg : config.init) {
      TRACEF("%.*s ", static_cast<int>(arg.size()), arg.data());
    }
    TRACEF("\n");
  }

  auto pre_run = [&](CurrentTask& init_task) -> fit::result<Errno> {
    auto stdio = SyslogFile::new_file(init_task);
    auto files = init_task->files();
    for (int i : {0, 1, 2}) {
      if (files.get(FdNumber::from_raw(i)).is_error()) {
        auto result = files.insert(*init_task.task(), FdNumber::from_raw(i), stdio) _EP(result);
      }
    }

    fbl::AllocChecker ac;
    fbl::Vector<BString> envp;
    // envp.push_back("LD_DEBUG=all", &ac);
    // ZX_ASSERT(ac.check());
    // envp.push_back("LD_SHOW_AUXV=1", &ac);
    // ZX_ASSERT(ac.check());
    envp.push_back("LD_LIBRARY_PATH=/usr/lib", &ac);
    ZX_ASSERT(ac.check());
    envp.push_back("HOME=/", &ac);
    ZX_ASSERT(ac.check());
    envp.push_back("TERM=linux", &ac);
    ZX_ASSERT(ac.check());
    return init_task.exec(executable.value(), argv[0], argv, envp);
  };

  auto task_complete = [](fit::result<zx_status_t>) -> void {
    LTRACEF("Finished running init process.\n");
  };

  execute_task_with_prerun_result(ktl::move(init_task.value()), pre_run, task_complete);

  return fit::ok(Container{kernel});
}

fit::result<zx_status_t, fbl::RefPtr<FsContext>> create_fs_context(
    const fbl::RefPtr<Kernel>& kernel, const Config& config) {
  auto rootfs = BootFs::new_fs(kernel, GetZbi());
  return fit::ok(ktl::move(FsContext::New(Namespace::new_with_flags(rootfs, MountFlags::empty()))));
}

}  // namespace starnix
