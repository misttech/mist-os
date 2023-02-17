// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_BRINGUP_BIN_NETSVC_TEST_PAVER_TEST_COMMON_H_
#define SRC_BRINGUP_BIN_NETSVC_TEST_PAVER_TEST_COMMON_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire_test_base.h>
#include <fidl/fuchsia.paver/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/devmgr-integration-test/fixture.h>
#include <lib/fdio/fd.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/netboot/netboot.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/channel.h>
#include <lib/zx/time.h>
#include <threads.h>
#include <zircon/errors.h>
#include <zircon/time.h>

#include <algorithm>
#include <memory>
#include <vector>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <ramdevice-client/ramdisk.h>
#include <zxtest/zxtest.h>

#include "src/bringup/bin/netsvc/paver.h"
#include "src/storage/testing/fake-paver.h"

class FakeFshost : public fidl::testing::WireTestBase<fuchsia_fshost::Admin> {
 public:
  void WriteDataFile(WriteDataFileRequestView request,
                     WriteDataFileCompleter::Sync& completer) override {
    data_file_path_ = request->filename.get();
    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FAIL("Unexpected call to fuchsia.fshost.Admin: %s", name.c_str());
  }

  const std::string& data_file_path() const { return data_file_path_; }

 private:
  std::string data_file_path_;
};

class FakeSvc {
 public:
  explicit FakeSvc(async_dispatcher_t* dispatcher) : outgoing_(dispatcher) {
    ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_paver::Paver>(
        [this, dispatcher](fidl::ServerEnd<fuchsia_paver::Paver> server_end) {
          ASSERT_OK(fake_paver_.Connect(dispatcher, std::move(server_end)));
        }));
    ASSERT_OK(outgoing_.AddUnmanagedProtocol<fuchsia_fshost::Admin>(
        [this, dispatcher](fidl::ServerEnd<fuchsia_fshost::Admin> server_end) {
          ASSERT_OK(fidl::BindSingleInFlightOnly(dispatcher, std::move(server_end), &fake_fshost_));
        }));

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    auto& [client_end, server_end] = endpoints.value();
    zx::result result = outgoing_.Serve(std::move(server_end));
    ASSERT_OK(result);
    root_.Bind(std::move(client_end));
  }

  paver_test::FakePaver& fake_paver() { return fake_paver_; }
  FakeFshost& fake_fshost() { return fake_fshost_; }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    auto& [client_end, server_end] = endpoints.value();
    const fidl::OneWayStatus status =
        root_->Open({}, {}, component::OutgoingDirectory::kServiceDirectory,
                    fidl::ServerEnd<fuchsia_io::Node>{server_end.TakeChannel()});
    return zx::make_result(status.status(), std::move(client_end));
  }

 private:
  component::OutgoingDirectory outgoing_;
  paver_test::FakePaver fake_paver_;
  FakeFshost fake_fshost_;
  fidl::WireSyncClient<fuchsia_io::Directory> root_;
};

class FakeDev {
 public:
  explicit FakeDev(async_dispatcher_t* dispatcher) {
    auto args = devmgr_integration_test::IsolatedDevmgr::DefaultArgs();
    args.root_device_driver = "/boot/driver/platform-bus.so";

    zx::result devmgr =
        devmgr_integration_test::IsolatedDevmgr::Create(std::move(args), dispatcher);
    ASSERT_OK(devmgr.status_value());
    devmgr_ = std::move(devmgr.value());
    // TODO(https://fxbug.dev/80815): Stop requiring this recursive wait.
    ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr_.devfs_root().get(),
                                                   "sys/platform/00:00:2d/ramctl")
                  .status_value());
  }

  devmgr_integration_test::IsolatedDevmgr devmgr_;
};

class PaverTest : public zxtest::Test {
 protected:
  PaverTest()
      : loop_(&kAsyncLoopConfigNeverAttachToThread),
        paver_(
            [this]() {
              zx::channel client;
              EXPECT_OK(fdio_fd_clone(fake_dev_.devmgr_.devfs_root().get(),
                                      client.reset_and_get_address()));
              return fidl::ClientEnd<fuchsia_io::Directory>{std::move(client)};
            }(),
            [this]() {
              zx::result svc = fake_svc_.svc();
              EXPECT_OK(svc);
              return std::move(svc.value());
            }()) {}

  ~PaverTest() override {
    // Need to make sure paver thread exits.
    paver_.exit_code().wait();
    if (ramdisk_ != nullptr) {
      ramdisk_destroy(ramdisk_);
      ramdisk_ = nullptr;
    }
  }

  void SpawnBlockDevice() {
    ASSERT_OK(device_watcher::RecursiveWaitForFile(fake_dev_.devmgr_.devfs_root().get(),
                                                   "sys/platform/00:00:2d/ramctl")
                  .status_value());
    ASSERT_OK(ramdisk_create_at(fake_dev_.devmgr_.devfs_root().get(), zx_system_get_page_size(),
                                100, &ramdisk_));
    std::string expected = std::string("/dev/") + ramdisk_get_path(ramdisk_);
    fake_svc_.fake_paver().set_expected_device(expected);
  }

  async::Loop loop_;
  ramdisk_client_t* ramdisk_ = nullptr;
  FakeSvc fake_svc_{loop_.dispatcher()};
  FakeDev fake_dev_{loop_.dispatcher()};
  netsvc::Paver paver_;
};

#endif  // SRC_BRINGUP_BIN_NETSVC_TEST_PAVER_TEST_COMMON_H_
