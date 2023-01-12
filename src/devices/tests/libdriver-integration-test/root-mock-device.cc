// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "root-mock-device.h"

#include <fcntl.h>
#include <fuchsia/device/cpp/fidl.h>
#include <fuchsia/device/test/cpp/fidl.h>
#include <lib/devmgr-integration-test/fixture.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/unsafe.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>

#include "lib/stdcompat/string_view.h"

#define DRIVER_TEST_DIR "/boot/driver"
#define MOCK_DEVICE_LIB "/boot/driver/mock-device.so"

namespace libdriver_integration_test {

RootMockDevice::RootMockDevice(std::unique_ptr<MockDeviceHooks> hooks,
                               fidl::InterfacePtr<fuchsia::device::test::Device> test_device,
                               fidl::InterfaceRequest<fuchsia::device::mock::MockDevice> controller,
                               async_dispatcher_t* dispatcher, std::string path)
    : test_device_(std::move(test_device)),
      path_(std::move(path)),
      mock_(std::move(controller), dispatcher, "") {
  mock_.set_hooks(std::move(hooks));
}

RootMockDevice::~RootMockDevice() {
  // This will trigger unbind() to be called on any device that was added in
  // the bind hook.
  test_device_->Destroy();
}

// |*test_device_out| will be a channel to the test device that the mock device
// bound to.  This is provided so we can trigger unbinding of the mock device.
// |*control_out| will be a channel for fulfilling requests from the mock
// device.
zx_status_t RootMockDevice::Create(const IsolatedDevmgr& devmgr, async_dispatcher_t* dispatcher,
                                   std::unique_ptr<MockDeviceHooks> hooks,
                                   std::unique_ptr<RootMockDevice>* mock_out) {
  // Wait for /dev/sys/test/test to appear
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/test/test");
  if (channel.is_error()) {
    return channel.status_value();
  }

  zx::channel test_root_chan = std::move(channel.value());

  fidl::SynchronousInterfacePtr<fuchsia::device::test::RootDevice> test_root;
  test_root.Bind(std::move(test_root_chan));

  return CreateFromTestRoot(devmgr, dispatcher, std::move(test_root), std::move(hooks), mock_out);
}

zx_status_t RootMockDevice::CreateFromTestRoot(
    const IsolatedDevmgr& devmgr, async_dispatcher_t* dispatcher,
    fidl::SynchronousInterfacePtr<fuchsia::device::test::RootDevice> test_root,
    std::unique_ptr<MockDeviceHooks> hooks, std::unique_ptr<RootMockDevice>* mock_out) {
  const std::string kName = "mock";

  fuchsia::device::test::RootDevice_CreateDevice_Result create_result;
  if (zx_status_t status = test_root->CreateDevice(kName, &create_result); status != ZX_OK) {
    return status;
  }
  if (create_result.is_err()) {
    return create_result.err();
  }

  // Ignore the |devpath| return and construct it ourselves, since the test
  // driver makes an assumption about where it's bound which isn't true in the
  // case where we're testing composite devices
  fidl::SynchronousInterfacePtr<fuchsia::device::Controller> test_root_controller;
  test_root_controller.Bind(test_root.Unbind().TakeChannel());
  fuchsia::device::Controller_GetTopologicalPath_Result result;
  if (zx_status_t status = test_root_controller->GetTopologicalPath(&result); status != ZX_OK) {
    return status;
  }
  if (result.is_err()) {
    return result.err();
  }
  fidl::StringPtr path_opt = result.response().path;
  if (!path_opt.has_value()) {
    return ZX_ERR_BAD_STATE;
  }
  std::string path = std::move(path_opt.value());
  constexpr std::string_view kDevPrefix = "/dev/";
  if (!cpp20::starts_with(std::string_view{path}, kDevPrefix)) {
    return ZX_ERR_BAD_STATE;
  }
  path.erase(0, kDevPrefix.length());
  path.append("/");
  path.append(kName);

  // Connect to the created device.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), path.c_str());
  if (channel.is_error()) {
    return channel.error_value();
  }
  fidl::SynchronousInterfacePtr<fuchsia::device::test::Device> test_dev;
  test_dev.Bind(std::move(channel.value()));

  auto destroy_device = fit::defer([&test_dev] { test_dev->Destroy(); });

  fidl::InterfaceHandle<fuchsia::device::mock::MockDevice> client;
  fidl::InterfaceRequest<fuchsia::device::mock::MockDevice> server(client.NewRequest());
  if (!server.is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  if (zx_status_t status = test_dev->SetChannel(client.TakeChannel()); status != ZX_OK) {
    return status;
  }

  // Open a new connection to the test device to return.  We do to simplify
  // handling around the blocking nature of fuchsia.device.Controller/Bind.  Needs to
  // happen before the bind(), since bind() will cause us to get blocked in the mock device
  // driver waiting for input on what to do.
  //
  // TODO(https://fxbug.dev/112484): this relies on multiplexing.
  fidl::InterfacePtr<fuchsia::device::test::Device> test_device;
  fidl::SynchronousInterfacePtr<fuchsia::device::Controller> controller;
  {
    fidl::SynchronousInterfacePtr node =
        fidl::InterfaceHandle<fuchsia::io::Node>(test_dev.Unbind().TakeChannel()).BindSync();
    if (zx_status_t status = node->Clone(fuchsia::io::OpenFlags::CLONE_SAME_RIGHTS,
                                         fidl::InterfaceRequest<fuchsia::io::Node>(
                                             test_device.NewRequest(dispatcher).TakeChannel()));
        status != ZX_OK) {
      return status;
    }
    controller.Bind(node.Unbind().TakeChannel());
  }

  // Bind the mock device driver in a separate thread, since this call is
  // synchronous.
  std::thread thrd{[controller = std::move(controller)]() {
    fuchsia::device::Controller_Bind_Result result;
    zx_status_t status = controller->Bind(MOCK_DEVICE_LIB, &result);
    ZX_ASSERT_MSG(status == ZX_OK, "%s", zx_status_get_string(status));
    // BasicLifecycleTest.BindError.
    if (result.is_err()) {
      ZX_ASSERT_MSG(result.err() == ZX_ERR_NOT_SUPPORTED, "%s", zx_status_get_string(result.err()));
    }
  }};
  thrd.detach();

  destroy_device.cancel();

  auto mock = std::make_unique<RootMockDevice>(std::move(hooks), std::move(test_device),
                                               std::move(server), dispatcher, std::move(path));
  *mock_out = std::move(mock);
  return ZX_OK;
}

}  // namespace libdriver_integration_test
