// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <inttypes.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/io.h>
#include <lib/fdio/watcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fit/defer.h>
#include <lib/fpromise/promise.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <condition_variable>
#include <ostream>

#include <fbl/string.h>
#include <fbl/unique_fd.h>
#include <ramdevice-client/ramdisk.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "src/security/lib/fcrypto/digest.h"
#include "src/security/lib/fcrypto/secret.h"
#include "src/security/lib/zxcrypt/client.h"

namespace {
constexpr zx::duration kTimeout = zx::sec(3);
constexpr uint32_t kBlockSz = 512;
constexpr uint32_t kBlockCnt = 20;

class ZxcryptInspect : public gtest::RealLoopFixture {
 public:
  void SetUp() override {
    // Zxcrypt volume manager requires this.
    driver_integration_test::IsolatedDevmgr::Args args;
    ASSERT_EQ(driver_integration_test::IsolatedDevmgr::Create(&args, &devmgr_), ZX_OK);
    ASSERT_EQ(device_watcher::RecursiveWaitForFile(devmgr_.devfs_root().get(),
                                                   "sys/platform/ram-disk/ramctl")
                  .status_value(),
              ZX_OK);
  }

  fpromise::promise<inspect::Hierarchy> ReadInspect() {
    using diagnostics::reader::ArchiveReader;
    using diagnostics::reader::SanitizeMonikerForSelectors;

    const std::string moniker =
        std::string{"realm_builder:"} + devmgr_.RealmChildName() +
        "/driver_test_realm/realm_builder:0/boot-drivers:dev.sys.platform.ram-disk.ramctl.ramdisk-0.block";

    return fpromise::make_ok_promise(
               std::unique_ptr<ArchiveReader>(new ArchiveReader(
                   dispatcher(), {SanitizeMonikerForSelectors(moniker) + ":root"})))
        .and_then([moniker = std::move(moniker)](std::unique_ptr<ArchiveReader>& reader) {
          return reader->SnapshotInspectUntilPresent({moniker}).then(
              [](fpromise::result<std::vector<diagnostics::reader::InspectData>, std::string>&
                     wrapped) {
                EXPECT_TRUE(wrapped.is_ok()) << wrapped.take_error();
                auto data = wrapped.take_value();
                EXPECT_EQ(1ul, data.size());
                auto& inspect_data = data[0];
                if (!inspect_data.payload().has_value()) {
                  FX_LOGS(WARNING) << "inspect_data had nullopt payload";
                }
                if (inspect_data.payload().has_value() &&
                    inspect_data.payload().value() == nullptr) {
                  FX_LOGS(WARNING) << "inspect_data had nullptr for payload";
                }
                if (inspect_data.metadata().errors.has_value()) {
                  for (const auto& e : inspect_data.metadata().errors.value()) {
                    FX_LOGS(WARNING) << e.message;
                  }
                }

                return fpromise::ok(inspect_data.TakePayload());
              });
        });
  }

  std::string GetInspectInstanceGuid() {
    std::optional<inspect::Hierarchy> base_hierarchy;
    async::Executor exec(dispatcher());
    exec.schedule_task(ReadInspect().and_then(
        [&](inspect::Hierarchy& h) { base_hierarchy.emplace(std::move(h)); }));
    RunLoopUntil([&] { return base_hierarchy.has_value(); });
    auto* hierarchy = base_hierarchy->GetByPath({"zxcrypt0x0"});
    if (hierarchy == nullptr) {
      return "";
    }
    auto* property = hierarchy->node().get_property<inspect::StringPropertyValue>("instance_guid");
    if (property == nullptr) {
      return "";
    }
    return property->value();
  }

  const driver_integration_test::IsolatedDevmgr& devmgr() { return devmgr_; }

 private:
  driver_integration_test::IsolatedDevmgr devmgr_;
};

TEST_F(ZxcryptInspect, ExportsGuid) {
  fbl::unique_fd devfs_root_fd = devmgr().devfs_root().duplicate();

  // Create a new ramdisk to stick our zxcrypt instance on.
  ramdisk_client_t* ramdisk = nullptr;
  ASSERT_EQ(ZX_OK, ramdisk_create_at(devmgr().devfs_root().get(), kBlockSz, kBlockCnt, &ramdisk));
  auto cleanup = fit::defer([ramdisk]() { ASSERT_EQ(ZX_OK, ramdisk_destroy(ramdisk)); });
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root_fd.get(), ramdisk_get_path(ramdisk));
  ASSERT_EQ(ZX_OK, channel.status_value());
  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_device::Controller>::Create();
  {
    fidl::UnownedClientEnd<fuchsia_device::Controller> client(
        ramdisk_get_block_controller_interface(ramdisk));
    ASSERT_EQ(
        ZX_OK,
        fidl::WireCall(client)->ConnectToController(std::move(controller_server_end)).status());
  }

  // Create a new zxcrypt volume manager using the ramdisk.
  zxcrypt::VolumeManager vol_mgr(std::move(controller_client_end), std::move(devfs_root_fd));
  zx::channel zxc_client_chan;
  ASSERT_EQ(ZX_OK, vol_mgr.OpenClient(kTimeout, zxc_client_chan));

  // Create a new crypto key.
  crypto::Secret key;
  size_t digest_len;
  ASSERT_EQ(ZX_OK, crypto::digest::GetDigestLen(crypto::digest::kSHA256, &digest_len));
  ASSERT_EQ(ZX_OK, key.Generate(digest_len));

  // Use a new connection rather than using devfs_root because devfs_root has the empty set of
  // rights.
  const fidl::ClientEnd svc = devmgr().RealmExposedDir();

  // Unsealing should fail right now until we format. It'll look like a bad key error, but really we
  // haven't even got a formatted device yet.
  zxcrypt::EncryptedVolumeClient volume_client(std::move(zxc_client_chan));
  ASSERT_EQ(volume_client.Unseal(key.get(), key.len(), 0), ZX_ERR_ACCESS_DENIED);
  ASSERT_TRUE(GetInspectInstanceGuid().empty());

  // After formatting, we should be able to unseal a device and see its GUID in inspect.
  ASSERT_EQ(ZX_OK, volume_client.Format(key.get(), key.len(), 0));

  ASSERT_EQ(ZX_OK, volume_client.Unseal(key.get(), key.len(), 0));
  std::string guid = GetInspectInstanceGuid();
  ASSERT_FALSE(guid.empty());

  ASSERT_EQ(ZX_OK, volume_client.Seal());
}

}  // namespace
