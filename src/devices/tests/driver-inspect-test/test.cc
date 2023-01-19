// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.inspect.test/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/async/cpp/executor.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/io.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <sdk/lib/inspect/testing/cpp/zxtest/inspect.h>
#include <zxtest/zxtest.h>

namespace {
using driver_integration_test::IsolatedDevmgr;
using fuchsia_device_inspect_test::TestInspect;
using inspect::InspectTestHelper;

class InspectTestCase : public InspectTestHelper, public zxtest::Test {
 public:
  ~InspectTestCase() override = default;

  void SetUp() override {
    IsolatedDevmgr::Args args;

    args.device_list.push_back({
        .vid = PDEV_VID_TEST,
        .pid = PDEV_PID_INSPECT_TEST,
        .did = 0,
    });

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));

    zx::result channel = device_watcher::RecursiveWaitForFile(devmgr_.devfs_root().get(),
                                                              "sys/platform/11:13:0/inspect-test");
    ASSERT_OK(channel.status_value());
    client_ = fidl::ClientEnd<TestInspect>{std::move(channel.value())};
  }

  const IsolatedDevmgr& devmgr() { return devmgr_; }
  const fidl::ClientEnd<TestInspect>& client() { return client_; }

 private:
  IsolatedDevmgr devmgr_;
  fidl::ClientEnd<TestInspect> client_;
};

TEST_F(InspectTestCase, InspectDevfs) {
  // Check if inspect-test device is hosted in diagnostics folder
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr().devfs_root().get(), "diagnostics/class"));
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr().devfs_root().get(),
                                                 "diagnostics/class/test/000.inspect"));
}

TEST_F(InspectTestCase, ReadInspectData) {
  // Wait for inspect data to appear
  zx::result inspect_channel = device_watcher::RecursiveWaitForFile(
      devmgr().devfs_root().get(), "diagnostics/class/test/000.inspect");
  ASSERT_OK(inspect_channel);
  fbl::unique_fd fd;
  ASSERT_OK(fdio_fd_create(inspect_channel.value().release(), fd.reset_and_get_address()));
  {
    zx::vmo vmo;
    ASSERT_OK(fdio_get_vmo_clone(fd.get(), vmo.reset_and_get_address()));

    // Check initial inspect data
    ASSERT_NO_FATAL_FAILURE(ReadInspect(vmo));
    // testBeforeDdkAdd: "OK"
    ASSERT_NO_FATAL_FAILURE(
        CheckProperty(hierarchy().node(), "testBeforeDdkAdd", inspect::StringPropertyValue("OK")));
  }

  // Call test-driver to modify inspect data
  const fidl::WireResult result = fidl::WireCall(client())->ModifyInspect();
  ASSERT_OK(result);
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));

  // Verify new inspect data is reflected
  {
    zx::vmo vmo;
    ASSERT_OK(fdio_get_vmo_clone(fd.get(), vmo.reset_and_get_address()));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(vmo));
    // Previous values
    ASSERT_NO_FATAL_FAILURE(
        CheckProperty(hierarchy().node(), "testBeforeDdkAdd", inspect::StringPropertyValue("OK")));
    // New addition - testModify: "OK"
    ASSERT_NO_FATAL_FAILURE(
        CheckProperty(hierarchy().node(), "testModify", inspect::StringPropertyValue("OK")));
  }
}

}  // namespace
