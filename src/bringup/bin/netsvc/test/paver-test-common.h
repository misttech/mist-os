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
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/devmgr-integration-test/fixture.h>
#include <lib/fdio/fd.h>
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
  FakeFshost() = default;
  FakeFshost(const FakeFshost&) = delete;
  FakeFshost& operator=(const FakeFshost&) = delete;

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
  explicit FakeSvc(async_dispatcher_t* dispatcher) {
    zx::result server_end = fidl::CreateEndpoints(&root_);
    ASSERT_OK(server_end);
    async::PostTask(dispatcher, [dispatcher, &fake_paver = fake_paver_, &fake_fshost = fake_fshost_,
                                 server_end = std::move(server_end.value())]() mutable {
      component::OutgoingDirectory outgoing{dispatcher};
      ASSERT_OK(outgoing.AddUnmanagedProtocol<fuchsia_paver::Paver>(
          [&fake_paver, dispatcher](fidl::ServerEnd<fuchsia_paver::Paver> server_end) mutable {
            fake_paver.Connect(dispatcher, std::move(server_end));
          }));
      ASSERT_OK(outgoing.AddUnmanagedProtocol<fuchsia_fshost::Admin>(
          [&fake_fshost, dispatcher](fidl::ServerEnd<fuchsia_fshost::Admin> server_end) {
            fidl::BindServer(dispatcher, std::move(server_end), &fake_fshost);
          }));
      zx::result result = outgoing.Serve(std::move(server_end));
      ASSERT_OK(result);
      // Stash the outgoing directory on the dispatcher so that the dtor runs on the dispatcher
      // thread.
      async::PostDelayedTask(
          dispatcher, [outgoing = std::move(outgoing)]() {}, zx::duration::infinite());
    });
  }

  paver_test::FakePaver& fake_paver() { return fake_paver_; }
  FakeFshost& fake_fshost() { return fake_fshost_; }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc() {
    return component::ConnectAt<fuchsia_io::Directory>(
        root_, component::OutgoingDirectory::kServiceDirectory);
  }

 private:
  paver_test::FakePaver fake_paver_;
  FakeFshost fake_fshost_;
  fidl::ClientEnd<fuchsia_io::Directory> root_;
};

class FakeDev {
 public:
  explicit FakeDev(async_dispatcher_t* dispatcher) {
    auto args = devmgr_integration_test::IsolatedDevmgr::DefaultArgs();
    args.root_device_driver = "/boot/meta/platform-bus.cm";

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
