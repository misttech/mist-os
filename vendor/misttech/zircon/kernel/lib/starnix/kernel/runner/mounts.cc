// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/runner/mounts.h"

#include <lib/handoff/handoff.h>
#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/util/error_propagation.h>
#include <trace.h>

#include <utility>

#include "private.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_RUNNER_GLOBAL_TRACE(0)

namespace starnix_kernel_runner {

using starnix::BootFs;
using starnix::FileSystemOptions;
using starnix::MountParams;
using starnix::TmpFs;

fit::result<Error, MountAction> MountAction::new_for_root(
    const fbl::RefPtr<starnix::Kernel>& kernel, ktl::string_view spec) {
  auto parse_result = MountSpec::parse(spec) _EP(parse_result);
  auto [mount_spec, options] = parse_result.value();

  ZX_ASSERT(mount_spec.mount_point_ == "/");

  // We only support mounting these file systems at the root.
  // The root file system needs to be creatable without a task because we mount the root
  // file system before creating the initial task.
  starnix::FileSystemHandle fs;
  if (mount_spec.fs_type_ == "bootfs") {
    fs = BootFs::new_fs(kernel, GetZbi());
  } else if (mount_spec.fs_type_ == "tmpfs") {
    auto result = TmpFs::new_fs_with_options(kernel, options) _EP(result);
    fs = result.value();
  } else {
    ZX_PANIC("unsupported root file system %.*s", static_cast<int>(mount_spec.fs_type_.size()),
             mount_spec.fs_type_.data());
  }

  return fit::ok(mount_spec.into_action(fs));
}

fit::result<Error, MountAction> MountAction::from_spec(const starnix::CurrentTask& current_task,
                                                       ktl::string_view spec) {
  LTRACEF("spec:%.*s\n", static_cast<int>(spec.size()), spec.data());

  auto parse_result = MountSpec::parse(spec) _EP(parse_result);
  auto [mount_spec, options] = parse_result.value();

  starnix::FileSystemHandle fs;
  if (mount_spec.fs_type_ == "bootfs") {
  } else {
    auto fs_result = current_task.create_filesystem(mount_spec.fs_type_, options) _EP(fs_result);
    fs = fs_result.value();
  }

  return fit::ok(mount_spec.into_action(fs));
}

fit::result<Error, ktl::pair<MountSpec, FileSystemOptions>> MountSpec::parse(
    ktl::string_view spec) {
  size_t pos = 0;
  size_t next_pos;
  ktl::array<ktl::string_view, 4> parts;
  int i = 0;

  while (i < 4 && (next_pos = spec.find(':', pos)) != ktl::string_view::npos) {
    parts[i++] = spec.substr(pos, next_pos - pos);
    pos = next_pos + 1;
  }

  if (i < 4) {
    parts[i++] = spec.substr(pos);
  }

  auto mount_point = !parts[0].empty() ? parts[0] : ktl::string_view();
  auto fs_type = i > 1 && !parts[1].empty() ? parts[1] : ktl::string_view();
  auto fs_src = i > 2 && !parts[2].empty() ? parts[2] : ".";

  if (mount_point.empty()) {
    return fit::error(anyhow("mount point is missing from %.*s", spec));
  }
  if (fs_type.empty()) {
    return fit::error(anyhow("fs type is missing from %.*s", spec));
  }

  auto mount_params = MountParams::parse(parts[3]) _EP(mount_params);
  auto flags = mount_params->remove_mount_flags();
  return fit::ok(ktl::pair<MountSpec, FileSystemOptions>(
      MountSpec(mount_point, fs_type, flags),
      FileSystemOptions{
          .source = fs_src,
          .flags = flags & MountFlagsEnum::STORED_ON_FILESYSTEM,
          .params = mount_params.value(),
      }));
}

MountAction MountSpec::into_action(starnix::FileSystemHandle fs) const {
  return MountAction{.path_ = mount_point_, .fs_ = ktl::move(fs), .flags_ = flags_};
}

}  // namespace starnix_kernel_runner
