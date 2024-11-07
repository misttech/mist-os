// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/device/mem.h>
#include <lib/mistos/starnix/kernel/device/registry.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/device_directory.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

bool registry_fails_to_add_duplicate_device() {
  BEGIN_TEST;

  auto registry = starnix::DeviceRegistry::Default();

  ASSERT_TRUE(registry
                  .register_major("mem", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  MEM_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "registers once");

  ASSERT_TRUE(registry
                  .register_major("random", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  123, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "registers unique");

  ASSERT_TRUE(registry
                  .register_major("mem", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  MEM_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_error(),
              "fail to register duplicate");

  END_TEST;
}

bool registry_opens_device() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  auto registry = starnix::DeviceRegistry::Default();

  EXPECT_TRUE(registry
                  .register_major("mem", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  MEM_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "registers device");

  fbl::AllocChecker ac;
  auto node = starnix::FsNode::new_root(new (&ac) starnix::testing::PanickingFsNode());
  ZX_ASSERT(ac.check());

  // Fail to open non-existent device.
  ASSERT_TRUE(
      registry
          .open_device(*current_task, *node, OpenFlags(OpenFlagsEnum::RDONLY), DeviceType::NONE,
                       starnix::DeviceMode(starnix::DeviceMode::Type::kChar))
          .is_error(),
      "opens registered device");

  // Fail to open in wrong mode.
  ASSERT_TRUE(registry
                  .open_device(*current_task, *node, OpenFlags(OpenFlagsEnum::RDONLY),
                               DeviceType::NILL,
                               starnix::DeviceMode(starnix::DeviceMode::Type::kBlock))
                  .is_error());

  // Open in correct mode.
  ASSERT_TRUE(
      registry
          .open_device(*current_task, *node, OpenFlags(OpenFlagsEnum::RDONLY), DeviceType::NILL,
                       starnix::DeviceMode(starnix::DeviceMode::Type::kChar))
          .is_ok(),
      "opens device");

  END_TEST;
}

bool registry_add_class() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto& registry = kernel->device_registry_;

  ASSERT_TRUE(registry
                  .register_major("input", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  INPUT_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "can register input");

  auto input_class =
      registry.objects_.get_or_create_class("input", registry.objects_.virtual_bus());
  registry.add_device<starnix::FsNodeOps>(
      *current_task, "mice",
      starnix::DeviceMetadata("mice", DeviceType::New(INPUT_MAJOR, 0),
                              starnix::DeviceMode(starnix::DeviceMode::Type::kChar)),
      input_class, starnix::DeviceDirectory::New);

  ASSERT_TRUE(registry.objects_.class_->has_child("input"));
  ASSERT_TRUE(registry.objects_.class_->get_child("input").has_value());
  ASSERT_TRUE(registry.objects_.class_->get_child("input").value()->has_child("mice"));

  END_TEST;
}

bool registry_add_bus() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto& registry = kernel->device_registry_;

  ASSERT_TRUE(registry
                  .register_major("input", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  INPUT_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "can register input");

  auto bus = registry.objects_.get_or_create_bus("bus");
  auto class_obj = registry.objects_.get_or_create_class("class", bus);
  registry.add_device<starnix::FsNodeOps>(
      *current_task, "device",
      starnix::DeviceMetadata("device", DeviceType::New(INPUT_MAJOR, 0),
                              starnix::DeviceMode(starnix::DeviceMode::Type::kChar)),
      class_obj, starnix::DeviceDirectory::New);

  ASSERT_TRUE(registry.objects_.bus_->has_child("bus"));
  ASSERT_TRUE(registry.objects_.bus_->get_child("bus").has_value());
  ASSERT_TRUE(registry.objects_.bus_->get_child("bus").value()->has_child("device"));

  END_TEST;
}

bool registry_remove_device() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto& registry = kernel->device_registry_;

  ASSERT_TRUE(registry
                  .register_major("input", starnix::DeviceMode(starnix::DeviceMode::Type::kChar),
                                  INPUT_MAJOR, starnix::simple_device_ops<starnix::DevNull>())
                  .is_ok(),
              "can register input");

  auto pci_bus = registry.objects_.get_or_create_bus("pci");
  auto input_class = registry.objects_.get_or_create_class("input", pci_bus);
  auto mice_dev = registry.add_device<starnix::FsNodeOps>(
      *current_task, "mice",
      starnix::DeviceMetadata("mice", DeviceType::New(INPUT_MAJOR, 0),
                              starnix::DeviceMode(starnix::DeviceMode::Type::kChar)),
      input_class, starnix::DeviceDirectory::New);

  registry.remove_device(*current_task, mice_dev);

  ASSERT_FALSE(input_class.kobject()->has_child("mice"));
  ASSERT_TRUE(registry.objects_.bus_->get_child("pci").has_value(), "get pci collection");
  ASSERT_FALSE(registry.objects_.bus_->get_child("pci").value()->has_child("mice"));
  ASSERT_TRUE(registry.objects_.class_->get_child("input").has_value(), "get input collection");
  ASSERT_FALSE(registry.objects_.class_->get_child("input").value()->has_child("mice"));

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_device_registry)
UNITTEST("registry fails to add duplicate device",
         unit_testing::registry_fails_to_add_duplicate_device)
UNITTEST("registry opens device", unit_testing::registry_opens_device)
UNITTEST("registry add class", unit_testing::registry_add_class)
UNITTEST("registry add bus", unit_testing::registry_add_bus)
UNITTEST("registry remove device", unit_testing::registry_remove_device)
UNITTEST_END_TESTCASE(starnix_device_registry, "starnix_device_registry",
                      "Tests for Device Registry")
