// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/runner/container.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
#include <lib/mistos/starnix/kernel/fs/mistos/syslog.h>
#include <lib/mistos/starnix/kernel/runner/mounts.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/starnix/modules/modules.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <object/process_dispatcher.h>
#include <object/vm_object_dispatcher.h>

#include "private.h"

namespace ktl::ranges {

using std::ranges::copy;

}

#include <algorithm>

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_RUNNER_GLOBAL_TRACE(0)

namespace starnix_kernel_runner {

using mtl::Error;

using starnix::CurrentTask;
using starnix::FdNumber;
using starnix::FsContext;
using starnix::Kernel;
using starnix::Namespace;
using starnix::SyslogFile;
using starnix::WhatToMount;

Container::~Container() { LTRACE_ENTRY_OBJ; }

namespace {

fit::result<Error> mount_filesystems(const CurrentTask& system_task, const Config& config) {
  // Skip the first mount, that was used to create the root filesystem
  auto mounts_iter = config.mounts.begin();
  if (mounts_iter != config.mounts.end()) {
    ++mounts_iter;
  }

  for (; mounts_iter != config.mounts.end(); ++mounts_iter) {
    const auto& mount_spec = *mounts_iter;

    auto action_result =
        starnix_uapi::make_source_context(MountAction::from_spec(system_task, mount_spec))
            .with_source_context([&mount_spec]() {
              return mtl::format("creating filesystem from spec: %.*s",
                                 static_cast<int>(mount_spec.size()), mount_spec.data());
            }) _EP(action_result);
    auto action = action_result.value();

    auto mount_point_result =
        starnix_uapi::make_source_context(system_task.lookup_path_from_root(action.path_))
            .with_source_context([&action]() {
              return mtl::format("lookup path from root: %.*s",
                                 static_cast<int>(action.path_.size()), action.path_.data());
            }) _EP(mount_point_result);
    auto mount_point = mount_point_result.value();

    _EP(mount_point.mount(WhatToMount::Fs(action.fs_), action.flags_));
  }

  return fit::ok();
}

}  // namespace

fit::result<Error, Container> create_container(const Config& config) {
  const ktl::string_view DEFAULT_INIT("/container/init");

  auto kernel = starnix_uapi::make_source_context(Kernel::New(config.kernel_cmdline))
                    .with_source_context([&config]() {
                      return mtl::format("creating Kernel: %.*s",
                                         static_cast<int>(config.name.size()), config.name.data());
                    }) _EP(kernel);

  auto fs_context = starnix_uapi::make_source_context(create_fs_context(kernel.value(), config))
                        .source_context("creating FsContext") _EP(fs_context);

  auto init_pid = kernel->pids_.Write()->allocate_pid();
  // Lots of software assumes that the pid for the init process is 1.
  DEBUG_ASSERT(init_pid == 1);

  {
    auto system_task = starnix_uapi::make_source_context(
                           CurrentTask::create_system_task(kernel.value(), fs_context.value()))
                           .source_context("create system task") _EP(system_task);
    // The system task gives pid 2. This value is less critical than giving
    // pid 1 to init, but this value matches what is supposed to happen.
    DEBUG_ASSERT(system_task->id_ == 2);

    _EP(starnix_uapi::make_source_context(kernel->kthreads_.Init(ktl::move(system_task.value())))
            .source_context("initializing kthreads"));
  }

  auto& system_task = kernel->kthreads_.system_task();

  // Register common devices and add them in sysfs and devtmpfs.
  starnix_modules::init_common_devices(system_task);
  starnix_modules::register_common_file_systems(kernel.value());

  _EP(starnix_uapi::make_source_context(mount_filesystems(system_task, config))
          .source_context("mounting filesystems"));

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
      ktl::ranges::copy(config.init, util::back_inserter(argv));
    }
    return ktl::move(argv);
  }();

  auto executable =
      system_task.open_file(argv[0], OpenFlags(OpenFlagsEnum::RDONLY)) _EP(executable);

  ktl::string_view initial_name;
  if (!config.init.is_empty()) {
    initial_name = config.init[0];
  }

  // let rlimits = parse_rlimits(&config.rlimits)?;
  fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>> rlimits;
  auto init_task = starnix_uapi::make_source_context(
                       CurrentTask::create_init_process(kernel.value(), init_pid, initial_name,
                                                        fs_context.value(), ktl::move(rlimits)))
                       .with_source_context([&config]() {
                         auto vec_to_str = mtl::to_string(config.init);
                         return mtl::format("creating init task: %.*s",
                                            static_cast<int>(vec_to_str.size()), vec_to_str.data());
                       }) _EP(init_task);

  auto pre_run = [&](CurrentTask& init_task) -> fit::result<Errno> {
    auto stdio = SyslogFile::new_file(init_task);
    auto files = init_task->files_;
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

  return fit::ok(Container{kernel.value()});
}

fit::result<Error, fbl::RefPtr<FsContext>> create_fs_context(const fbl::RefPtr<Kernel>& kernel,
                                                             const Config& config) {
  // The mounts are applied in the order listed. Mounting will fail if the designated mount
  // point doesn't exist in a previous mount. The root must be first so other mounts can be
  // applied on top of it.
  auto mounts_iter = config.mounts.begin();
  if (mounts_iter == config.mounts.end()) {
    return fit::error(anyhow("Mounts list is empty"));
  }

  auto root_result = MountAction::new_for_root(kernel, *mounts_iter) _EP(root_result);
  auto root = root_result.value();

  if (root.path_ != "/") {
    ZX_PANIC("First mount in mounts list is not the root");
  }

  return fit::ok(
      ktl::move(FsContext::New(Namespace::new_with_flags(root.fs_, MountFlags::empty()))));
}

}  // namespace starnix_kernel_runner
