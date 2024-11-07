// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/device/mem.h"

#include <lib/mistos/starnix/kernel/device/registry.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/device_directory.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/anon_node.h>

namespace starnix {

FileHandle new_null_file(const CurrentTask& current_task, OpenFlags flags) {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) DevNull();
  ZX_ASSERT(ac.check());

  return Anon::new_file_extended(
      current_task, ktl::unique_ptr<DevNull>(ptr), flags,
      FsNodeInfo::new_factory(FileMode::from_bits(0666), FsCred::root()));
}

// Initialize memory devices like /dev/null, /dev/zero, etc
void mem_device_init(const CurrentTask& system_task) {
  auto& kernel = system_task->kernel();
  auto& registry = kernel->device_registry_;

  auto mem_class = registry.objects_.mem_class();

  registry.register_device<FsNodeOps>(
      system_task, "null",
      DeviceMetadata("null", DeviceType::NILL, DeviceMode(DeviceMode::Type::kChar)), mem_class,
      DeviceDirectory::New, simple_device_ops<DevNull>());

  registry.register_device<FsNodeOps>(
      system_task, "zero",
      DeviceMetadata("zero", DeviceType::ZERO, DeviceMode(DeviceMode::Type::kChar)), mem_class,
      DeviceDirectory::New, simple_device_ops<DevZero>());
  /*
      registry.register_device(
          *locked, system_task, "full",
          DeviceMetadata("full", DeviceType::FULL, DeviceMode(DeviceMode::Type::kChar)), mem_class,
          DeviceDirectory::New, simple_device_ops<DevFull>());

      registry.register_device(
          *locked, system_task, "random",
          DeviceMetadata("random", DeviceType::RANDOM, DeviceMode(DeviceMode::Type::kChar)),
      mem_class, DeviceDirectory::New, simple_device_ops<DevRandom>());

      registry.register_device(
          *locked, system_task, "urandom",
          DeviceMetadata("urandom", DeviceType::URANDOM, DeviceMode(DeviceMode::Type::kChar)),
          mem_class, DeviceDirectory::New, simple_device_ops<DevRandom>());*/
}

}  // namespace starnix
