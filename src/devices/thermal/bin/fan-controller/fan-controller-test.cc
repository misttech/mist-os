// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "fan-controller.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fdio/namespace.h>
#include <unistd.h>

#include <queue>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace {

class FakeWatcher : public fidl::Server<fuchsia_thermal::ClientStateWatcher> {
 public:
  explicit FakeWatcher(fidl::ServerEnd<fuchsia_thermal::ClientStateWatcher> server)
      : binding_(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this,
                 fidl::kIgnoreBindingClosure) {}
  ~FakeWatcher() {
    if (completer_) {
      completer_->Close(ZX_ERR_CANCELED);
      completer_.reset();
    }
  }

  void Watch(WatchCompleter::Sync& completer) override { completer_ = completer.ToAsync(); }

 private:
  friend class FakeClientStateServer;

  fidl::ServerBinding<fuchsia_thermal::ClientStateWatcher> binding_;

  std::optional<WatchCompleter::Async> completer_;
};

class FakeClientStateServer : public fidl::Server<fuchsia_thermal::ClientStateConnector> {
 public:
  explicit FakeClientStateServer(fidl::ServerEnd<fuchsia_thermal::ClientStateConnector> server)
      : binding_(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this,
                 fidl::kIgnoreBindingClosure) {}
  ~FakeClientStateServer() override { EXPECT_TRUE(expected_connect_.empty()); }

  void ExpectConnect(const std::string& client_type) { expected_connect_.emplace(client_type); }
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    EXPECT_FALSE(expected_connect_.empty());
    EXPECT_STREQ(expected_connect_.front(), request.client_type());
    expected_connect_.pop();
    watchers_.emplace(request.client_type(), std::move(request.watcher()));
  }

  void ReplyToWatch(const std::string& client_type, uint64_t state) {
    auto& watcher = watchers_.at(client_type);
    ASSERT_TRUE(watcher.completer_.has_value());
    watcher.completer_->Reply(state);
    watcher.completer_.reset();
  }

  bool watch_called(const std::string& client_type) {
    return watchers_.find(client_type) != watchers_.end() &&
           watchers_.at(client_type).completer_.has_value();
  }

 private:
  fidl::ServerBinding<fuchsia_thermal::ClientStateConnector> binding_;

  std::queue<const std::string> expected_connect_;
  std::map<std::string, FakeWatcher> watchers_;
};

class FakeFanDevice : public fidl::Server<fuchsia_hardware_fan::Device> {
 public:
  explicit FakeFanDevice(std::string client_type) : client_type_(std::move(client_type)) {}
  ~FakeFanDevice() override { EXPECT_TRUE(expected_set_fan_level_.empty()); }

  // fuchsia_hardware_fan.Device protocol implementation.
  void GetFanLevel(GetFanLevelCompleter::Sync& completer) override {
    completer.Reply({ZX_ERR_NOT_SUPPORTED, 0});
  }
  void SetFanLevel(SetFanLevelRequest& request, SetFanLevelCompleter::Sync& completer) override {
    EXPECT_FALSE(expected_set_fan_level_.empty());
    EXPECT_EQ(expected_set_fan_level_.front(), request.fan_level());
    expected_set_fan_level_.pop();
    completer.Reply(ZX_OK);
  }
  void GetClientType(GetClientTypeCompleter::Sync& completer) override {
    completer.Reply({client_type_});
  }

  fbl::RefPtr<fs::Service> AsService() {
    return fbl::MakeRefCounted<fs::Service>(
        [this](fidl::ServerEnd<fuchsia_hardware_fan::Device> server) {
          bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server),
                               this, fidl::kIgnoreBindingClosure);
          return ZX_OK;
        });
  }

  void ExpectSetFanLevel(uint32_t level) { expected_set_fan_level_.emplace(level); }

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_fan::Device> bindings_;
  const std::string client_type_;

  std::queue<uint32_t> expected_set_fan_level_;
};

class FanControllerTest : public zxtest::Test {
 public:
  void SetUp() override {
    ASSERT_TRUE(dir_ != nullptr);

    ASSERT_EQ(fdio_ns_get_installed(&ns_), ZX_OK);
    zx::channel channel0, channel1;

    // Serve up the emulated fan directory
    ASSERT_EQ(zx::channel::create(0, &channel0, &channel1), ZX_OK);
    ASSERT_EQ(vfs_.Serve(dir_, std::move(channel0), fs::VnodeConnectionOptions::ReadOnly()), ZX_OK);
    ASSERT_EQ(fdio_ns_bind(ns_, fan_controller::kFanDirectory, channel1.release()), ZX_OK);

    auto endpoints = fidl::Endpoints<fuchsia_thermal::ClientStateConnector>::Create();
    client_state_.emplace(std::move(endpoints.server));
    client_end_ = std::move(endpoints.client);
  }

  void TearDown() override {
    // Scoped directory entries have gone out of scope, but to avoid races we remove all entries.
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(),
                                           [this]() { dir_->RemoveAllEntries(); });
    EXPECT_OK(result);
    ASSERT_TRUE(dir_->IsEmpty());

    ASSERT_NE(ns_, nullptr);
    ASSERT_EQ(fdio_ns_unbind(ns_, fan_controller::kFanDirectory), ZX_OK);
  }

  void StartFanController() {
    fan_controller_ = std::make_unique<fan_controller::FanController>(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(client_end_));
  }

  // Holds a ref to a pseudo dir entry that removes the entry when this object goes out of scope.
  class ScopedDirent {
   public:
    ScopedDirent(std::string name, fbl::RefPtr<fs::PseudoDir> dir, async_dispatcher_t* dispatcher,
                 const std::string& client_type)
        : name_(std::move(name)),
          dir_(std::move(dir)),
          dispatcher_(dispatcher),
          fan_(dispatcher, std::in_place, client_type) {
      fan_.SyncCall(
          [&](FakeFanDevice* fan) { ASSERT_OK(dir_->AddEntry(name_, fan->AsService())); });
    }
    ~ScopedDirent() {
      auto result = fdf::RunOnDispatcherSync(dispatcher_, [this]() { dir_->RemoveEntry(name_); });
      ASSERT_OK(result);
    }

    void ExpectSetFanLevel(uint32_t level) {
      fan_.SyncCall(&FakeFanDevice::ExpectSetFanLevel, level);
    }

   private:
    std::string name_;
    fbl::RefPtr<fs::PseudoDir> dir_;
    async_dispatcher_t* dispatcher_;
    async_patterns::TestDispatcherBound<FakeFanDevice> fan_;
  };

  ScopedDirent AddDevice(const std::string& client_type) {
    return ScopedDirent(std::to_string(next_device_number_++), dir_,
                        dispatcher_->async_dispatcher(), client_type);
  }

  void WaitForDevice(const std::string& client_type, size_t count) {
    runtime_.RunUntil([this, client_type, count]() {
      return fan_controller_->controller_fan_count(client_type) == count;
    });
    runtime_.RunUntil([this, client_type]() {
      return client_state_.SyncCall(&FakeClientStateServer::watch_called, client_type);
    });
  }

  void ReplyToWatch(const std::string& client_type, uint32_t state) {
    runtime_.RunUntil([this, client_type]() {
      return client_state_.SyncCall(&FakeClientStateServer::watch_called, client_type);
    });
    client_state_.SyncCall(&FakeClientStateServer::ReplyToWatch, client_type, state);
    runtime_.RunUntil([this, client_type]() {
      return client_state_.SyncCall(&FakeClientStateServer::watch_called, client_type);
    });
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher dispatcher_ = runtime_.StartBackgroundDispatcher();

  fidl::ClientEnd<fuchsia_thermal::ClientStateConnector> client_end_;

  fdio_ns_t* ns_ = nullptr;
  uint32_t next_device_number_ = 0;
  fs::SynchronousVfs vfs_{dispatcher_->async_dispatcher()};
  // Note this _must_ be RefPtrs since vfs_ will try to AdoptRef on the raw pointer passed to it.
  fbl::RefPtr<fs::PseudoDir> dir_{fbl::MakeRefCounted<fs::PseudoDir>()};
  std::map<std::string, size_t> client_type_count_;

 protected:
  std::unique_ptr<fan_controller::FanController> fan_controller_;
  async_patterns::TestDispatcherBound<FakeClientStateServer> client_state_{
      dispatcher_->async_dispatcher()};
};

TEST_F(FanControllerTest, DeviceBeforeStart) {
  const std::string kClientType = "fan";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType);
  auto dev = AddDevice(kClientType);

  StartFanController();
  WaitForDevice(kClientType, 1);

  dev.ExpectSetFanLevel(3);
  ReplyToWatch(kClientType, 3);
}

TEST_F(FanControllerTest, DeviceAfterStart) {
  StartFanController();

  const std::string kClientType = "fan";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType);
  auto dev = AddDevice(kClientType);
  WaitForDevice(kClientType, 1);

  dev.ExpectSetFanLevel(3);
  ReplyToWatch(kClientType, 3);
}

TEST_F(FanControllerTest, MultipleDevicesSameClientType) {
  StartFanController();

  const std::string kClientType = "fan";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType);
  auto dev0 = AddDevice(kClientType);
  WaitForDevice(kClientType, 1);

  auto dev1 = AddDevice(kClientType);
  WaitForDevice(kClientType, 2);

  dev0.ExpectSetFanLevel(3);
  dev1.ExpectSetFanLevel(3);
  ReplyToWatch(kClientType, 3);
}

TEST_F(FanControllerTest, MultipleDevicesDifferentClientTypes) {
  StartFanController();

  const std::string kClientType0 = "fan0";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType0);
  auto dev0 = AddDevice(kClientType0);
  WaitForDevice(kClientType0, 1);

  const std::string kClientType1 = "fan1";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType1);
  auto dev1 = AddDevice(kClientType1);
  WaitForDevice(kClientType1, 1);

  dev0.ExpectSetFanLevel(3);
  ReplyToWatch(kClientType0, 3);

  dev1.ExpectSetFanLevel(2);
  ReplyToWatch(kClientType1, 2);
}

TEST_F(FanControllerTest, DeviceRemoval) {
  const std::string kClientType = "fan";
  client_state_.SyncCall(&FakeClientStateServer::ExpectConnect, kClientType);
  auto dev0 = AddDevice(kClientType);

  StartFanController();
  WaitForDevice(kClientType, 1);

  {
    auto dev1 = AddDevice(kClientType);
    WaitForDevice(kClientType, 2);

    dev0.ExpectSetFanLevel(3);
    dev1.ExpectSetFanLevel(3);
    ReplyToWatch(kClientType, 3);

    // Remove fan1 by letting it go out of scope. This expects an error log.
  }

  dev0.ExpectSetFanLevel(6);
  ReplyToWatch(kClientType, 6);
  EXPECT_EQ(fan_controller_->controller_fan_count(kClientType), 1);
}

}  // namespace
