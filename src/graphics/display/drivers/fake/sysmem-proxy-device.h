// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_PROXY_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_PROXY_DEVICE_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fit/thread_checker.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/channel.h>
#include <threads.h>
#include <zircon/types.h>

#include <cstdint>

#include <ddktl/device.h>
#include <ddktl/unbind-txn.h>

namespace sysmem_driver {
class Driver;
}  // namespace sysmem_driver

namespace display {

class SysmemProxyDevice;
using DdkDeviceType2 =
    ddk::Device<SysmemProxyDevice,
                ddk::Messageable<fuchsia_hardware_sysmem::DriverConnector>::Mixin, ddk::Unbindable>;

// SysmemProxyDevice is a replacement for sysmem_driver::Device, intended for use in tests.  Instead
// of instantiating a separate/hermetic Sysmem, SysmemProxyDevice connects to the allocator made
// available via the test-component's environment (i.e. "/svc/fuchsia.sysmem.Allocator").  This is
// useful for testing use-cases where multiple components must share the same allocator to negotiate
// which memory to use.  For example, consider a scenario where Scenic wishes to use Vulkan for
// image compositing, and then wishes to display the resulting image on the screen.  In order to do
// so, it must allocate an image which is acceptable both to Vulkan and the display driver.
class SysmemProxyDevice final : public DdkDeviceType2 {
 public:
  SysmemProxyDevice(zx_device_t* parent_device, sysmem_driver::Driver* parent_driver);

  zx_status_t Bind();

  //
  // The rest of the methods are only valid to call after Bind().
  //

  // Ddk mixin implementations.
  // Quits the async loop and joins with all spawned threads.  Note: this doesn't tear down
  // connections already made via SysmemConnect().  This is because these connections are made by
  // passing the channel handle to an external Sysmem service, after which SysmemProxyDevice has no
  // further knowledge of the connection.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease() { delete this; }

  // fuchsia_hardware_sysmem::DriverConnector impl
  //
  // Drivers (and fake drivers) shouldn't be using DriverConnector, but continue serving it for now
  // until we've removed all usages from drivers (DriverConnector is for sysmem-connector not for
  // drivers, which should use fuchsia_hardware_sysmem::Service::AllocatorV1 or AllocatorV2).
  void ConnectV1(ConnectV1RequestView request, ConnectV1Completer::Sync& completer) override;
  void ConnectV2(ConnectV2RequestView request, ConnectV2Completer::Sync& completer) override;
  void SetAuxServiceDirectory(SetAuxServiceDirectoryRequestView request,
                              SetAuxServiceDirectoryCompleter::Sync& completer) override;

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CloneServiceDirClientForTests();
  async_dispatcher_t* dispatcher() { return loop_.dispatcher(); }

 private:
  sysmem_driver::Driver* parent_driver_ = nullptr;
  inspect::Inspector inspector_;
  async::Loop loop_;
  thrd_t loop_thrd_;

  // std::optional<> so we can init on the loop_ thread
  std::optional<component::OutgoingDirectory> service_outgoing_;
  fidl::ClientEnd<fuchsia_io::Directory> service_client_for_tests_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_PROXY_DEVICE_H_
