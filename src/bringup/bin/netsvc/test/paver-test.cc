// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <zircon/errors.h>

#include <string>

#include <zxtest/zxtest.h>

#include "src/bringup/bin/netsvc/test/paver-test-common.h"

namespace {

constexpr char kFakeData[] = "lalala";

// Returns a full firmware filename for the given type (no type by default).
std::string FirmwareFilename(const std::string& type = "") {
  return NETBOOT_FIRMWARE_FILENAME_PREFIX + type;
}

TEST(PaverTest, GetSingleton) {
  fdio_ns_t* ns;
  ASSERT_OK(fdio_ns_get_installed(&ns));
  for (const char* path : {"/dev/", "/dev/class/network/"}) {
    SCOPED_TRACE(path);
    netsvc::Paver::Reset();
    zx::channel client, server;
    ASSERT_OK(zx::channel::create(0, &client, &server));
    ASSERT_OK(fdio_ns_bind(ns, path, client.release()));
    auto unbind = fit::defer([ns, path]() { ASSERT_OK(fdio_ns_unbind(ns, path)); });
    std::ignore = netsvc::Paver::Get();
  }
}

class PaverTest : public ::PaverTest {
 protected:
  void AssertExitCode(zx_status_t status) {
    std::shared_future fut = paver_.exit_code();
    while (fut.wait_for(std::chrono::nanoseconds::zero()) != std::future_status::ready) {
      ASSERT_OK(loop_.RunUntilIdle());
    }
    ASSERT_STATUS(status, fut.get());
  }
};

TEST_F(PaverTest, OpenWriteInvalidFile) {
  char invalid_file_name[32] = {};
  ASSERT_NE(paver_.OpenWrite(invalid_file_name, 0, zx::duration::infinite()), TFTP_NO_ERROR);
  paver_.Close();
}

TEST_F(PaverTest, OpenWriteInvalidSize) {
  ASSERT_NE(paver_.OpenWrite(FirmwareFilename(), 0, zx::duration::infinite()), TFTP_NO_ERROR);
}

TEST_F(PaverTest, OpenWriteValidFile) {
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  paver_.Abort();
}

TEST_F(PaverTest, OpenTwice) {
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_NE(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  paver_.Abort();
}

TEST_F(PaverTest, WriteWithoutOpen) {
  size_t size = sizeof(kFakeData);
  ASSERT_NE(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
}

TEST_F(PaverTest, WriteAfterClose) {
  size_t size = sizeof(kFakeData);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  paver_.Close();
  // TODO(surajmalhotra): Should we ensure this fails?
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  paver_.Abort();
}

TEST_F(PaverTest, TimeoutNoWrites) {
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  paver_.Abort();
  ASSERT_STATUS(ZX_ERR_CANCELED, paver_.exit_code().get());
}

TEST_F(PaverTest, TimeoutPartialWrite) {
  size_t size = sizeof(kFakeData);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  paver_.Abort();
  ASSERT_STATUS(ZX_ERR_CANCELED, paver_.exit_code().get());
}

void ValidateCommandTrace(const std::vector<paver_test::Command>& actual,
                          const std::vector<paver_test::Command>& expected) {
  ASSERT_EQ(actual.size(), expected.size());
  for (size_t i = 0; i < actual.size(); i++) {
    EXPECT_EQ(actual[i], expected[i], "paver_test::Command #%zu different from expected", i);
  }
}

void ValidateLastCommandTrace(const std::vector<paver_test::Command>& actual,
                              const std::vector<paver_test::Command>& expected) {
  ASSERT_GE(actual.size(), expected.size());
  for (size_t i = 0; i < expected.size(); i++) {
    EXPECT_EQ(actual[actual.size() - expected.size() + i], expected[i],
              "paver_test::Command #%zu different from expected", i);
  }
}

TEST_F(PaverTest, WriteCompleteSingle) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
}

TEST_F(PaverTest, WriteCompleteManySmallWrites) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(1024);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 1024, zx::duration::infinite()), TFTP_NO_ERROR);
  for (size_t offset = 0; offset < 1024; offset += sizeof(kFakeData)) {
    size = std::min(sizeof(kFakeData), 1024 - offset);
    ASSERT_EQ(paver_.Write(kFakeData, &size, offset), TFTP_NO_ERROR);
    ASSERT_EQ(size, std::min(sizeof(kFakeData), 1024 - offset));
  }
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
}

TEST_F(PaverTest, Overwrite) {
  size_t size = sizeof(kFakeData);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 2, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_NE(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  paver_.Abort();
  ASSERT_STATUS(ZX_ERR_CANCELED, paver_.exit_code().get());
}

TEST_F(PaverTest, CloseChannelBetweenWrites) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(2 * size);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), 2 * size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  loop_.Shutdown();
  ASSERT_EQ(paver_.Write(kFakeData, &size, size), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_STATUS(ZX_ERR_PEER_CLOSED, paver_.exit_code().get());
  paver_.Close();
}

TEST_F(PaverTest, WriteFirmwareA) {
  size_t size = sizeof(kFakeData);
  const char* file_name = NETBOOT_IMAGE_PREFIX NETBOOT_FIRMWAREA_HOST_FILENAME_PREFIX "tpl";
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(file_name, size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
}

TEST_F(PaverTest, WriteZirconA) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_ZIRCONA_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteAsset});
}

TEST_F(PaverTest, WriteVbMetaA) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_VBMETAA_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteAsset});
}

TEST_F(PaverTest, WriteFirmwareABWithABRSupported) {
  fake_svc_.fake_paver().set_supported_firmware_type("tpl");
  const char* file_names[] = {
      NETBOOT_IMAGE_PREFIX NETBOOT_FIRMWAREA_HOST_FILENAME_PREFIX "tpl",
      NETBOOT_IMAGE_PREFIX NETBOOT_FIRMWAREB_HOST_FILENAME_PREFIX "tpl",
  };

  size_t size = sizeof(kFakeData);

  for (auto file_name : file_names) {
    fake_svc_.fake_paver().set_abr_supported(true);
    fake_svc_.fake_paver().set_expected_payload_size(size);
    ASSERT_EQ(paver_.OpenWrite(file_name, size, zx::duration::infinite()), TFTP_NO_ERROR);
    ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
    ASSERT_EQ(size, sizeof(kFakeData));
    ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
    paver_.Close();
    ValidateLastCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                             {
                                 paver_test::Command::kInitializeAbr,
                                 paver_test::Command::kQueryActiveConfiguration,
                                 paver_test::Command::kSetConfigurationUnbootable,
                                 paver_test::Command::kWriteFirmware,
                                 paver_test::Command::kBootManagerFlush,
                             });
  }
}

TEST_F(PaverTest, WriteFirmwareRWithABRSupported) {
  fake_svc_.fake_paver().set_supported_firmware_type("tpl");
  const char* file_name = NETBOOT_IMAGE_PREFIX NETBOOT_FIRMWARER_HOST_FILENAME_PREFIX "tpl";
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(file_name, size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateLastCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                           {
                               paver_test::Command::kInitializeAbr,
                               paver_test::Command::kQueryActiveConfiguration,
                               paver_test::Command::kWriteFirmware,
                               paver_test::Command::kBootManagerFlush,
                           });
}

TEST_F(PaverTest, WriteZirconAWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_ZIRCONA_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kBootManagerFlush,
                       });
}

TEST_F(PaverTest, WriteZirconBWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_ZIRCONB_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kBootManagerFlush,
                       });
}

TEST_F(PaverTest, WriteZirconRWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_ZIRCONR_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kBootManagerFlush,
                       });
}

TEST_F(PaverTest, WriteVbMetaAWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_VBMETAA_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kSetConfigurationActive,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kDataSinkFlush,
                           paver_test::Command::kBootManagerFlush,
                       });
  ASSERT_FALSE(fake_svc_.fake_paver().abr_data().slot_a.unbootable);
  ASSERT_TRUE(fake_svc_.fake_paver().abr_data().slot_a.active);
  ASSERT_TRUE(fake_svc_.fake_paver().abr_data().slot_b.unbootable);
}

TEST_F(PaverTest, WriteVbMetaBWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_VBMETAB_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kSetConfigurationActive,
                           paver_test::Command::kSetConfigurationUnbootable,
                           paver_test::Command::kDataSinkFlush,
                           paver_test::Command::kBootManagerFlush,
                       });
  ASSERT_FALSE(fake_svc_.fake_paver().abr_data().slot_b.unbootable);
  ASSERT_TRUE(fake_svc_.fake_paver().abr_data().slot_b.active);
  ASSERT_TRUE(fake_svc_.fake_paver().abr_data().slot_a.unbootable);
}

TEST_F(PaverTest, WriteVbMetaRWithABRSupported) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_VBMETAR_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {
                           paver_test::Command::kInitializeAbr,
                           paver_test::Command::kQueryActiveConfiguration,
                           paver_test::Command::kWriteAsset,
                           paver_test::Command::kBootManagerFlush,
                       });
}

TEST_F(PaverTest, WriteZirconAWithABRSupportedTwice) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_abr_supported(true);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  std::vector<paver_test::Command> expected_per_time = {
      paver_test::Command::kInitializeAbr,
      paver_test::Command::kQueryActiveConfiguration,
      paver_test::Command::kSetConfigurationUnbootable,
      paver_test::Command::kWriteAsset,
      paver_test::Command::kBootManagerFlush,
  };
  std::vector<paver_test::Command> expected_accumulative;
  for (int i = 0; i < 2; i++) {
    ASSERT_EQ(paver_.OpenWrite(NETBOOT_ZIRCONA_FILENAME, size, zx::duration::infinite()),
              TFTP_NO_ERROR);
    ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
    ASSERT_EQ(size, sizeof(kFakeData));
    ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
    paver_.Close();
    expected_accumulative.insert(expected_accumulative.end(), expected_per_time.begin(),
                                 expected_per_time.end());
    ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(), expected_accumulative);
  }
}

TEST_F(PaverTest, WriteSshAuth) {
  size_t size = sizeof(kFakeData);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_SSHAUTH_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ASSERT_EQ(fake_svc_.fake_fshost().data_file_path(), "ssh/authorized_keys");
}

TEST_F(PaverTest, WriteFvmSparse) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FVM_SPARSE_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kWriteVolumes});
}

TEST_F(PaverTest, WriteFvm) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FVM_FILENAME, size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kWriteOpaqueVolume});
}

TEST_F(PaverTest, WriteFxfs) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FXFS_FILENAME, size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kWriteSparseVolume});
}

TEST_F(PaverTest, WriteFvmManySmallWrites) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(1024);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FVM_SPARSE_FILENAME, 1024, zx::duration::infinite()),
            TFTP_NO_ERROR);
  for (size_t offset = 0; offset < 1024; offset += sizeof(kFakeData)) {
    size = std::min(sizeof(kFakeData), 1024 - offset);
    ASSERT_EQ(paver_.Write(kFakeData, &size, offset), TFTP_NO_ERROR);
    ASSERT_EQ(size, std::min(sizeof(kFakeData), 1024 - offset));
  }
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kWriteVolumes});
}

TEST_F(PaverTest, InitializePartitionTables) {
  ASSERT_NO_FAILURES(SpawnBlockDevice());

  netboot_block_device_t partition_info = {};
  strcpy(partition_info.block_device_path, "/dev/");
  strcat(partition_info.block_device_path, ramdisk_get_path(ramdisk_));

  size_t size = sizeof(partition_info);
  ASSERT_EQ(
      paver_.OpenWrite(NETBOOT_INIT_PARTITION_TABLES_FILENAME, size, zx::duration::infinite()),
      TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(&partition_info, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(partition_info));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitPartitionTables});
}

TEST_F(PaverTest, WipePartitionTables) {
  ASSERT_NO_FAILURES(SpawnBlockDevice());

  netboot_block_device_t partition_info = {};
  strcpy(partition_info.block_device_path, "/dev/");
  strcat(partition_info.block_device_path, ramdisk_get_path(ramdisk_));

  size_t size = sizeof(partition_info);
  ASSERT_EQ(
      paver_.OpenWrite(NETBOOT_WIPE_PARTITION_TABLES_FILENAME, size, zx::duration::infinite()),
      TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(&partition_info, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(partition_info));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kWipePartitionTables});
}

TEST_F(PaverTest, WriteFirmwareNoType) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
  EXPECT_EQ(fake_svc_.fake_paver().last_firmware_type(), "");
}

TEST_F(PaverTest, WriteFirmwareSupportedType) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  fake_svc_.fake_paver().set_supported_firmware_type("foo");

  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename("foo"), size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
  EXPECT_EQ(fake_svc_.fake_paver().last_firmware_type(), "foo");
}

TEST_F(PaverTest, WriteFirmwareUnsupportedType) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  fake_svc_.fake_paver().set_supported_firmware_type("foo");

  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename("bar"), size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();
  // This should still return OK so that we just skip unknown firmware types.

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
  EXPECT_EQ(fake_svc_.fake_paver().last_firmware_type(), "bar");
}

TEST_F(PaverTest, WriteFirmwareFailure) {
  // Trigger an error by not writing enough data.
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size + 1);

  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(), size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_ERR_INVALID_ARGS));
  paver_.Close();
  // This should not return OK since an actual error occurred.

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
}

TEST_F(PaverTest, WriteFirmwareTypeMaxLength) {
  const std::string type(NETBOOT_FIRMWARE_TYPE_MAX_LENGTH - 1, 'a');  // Max length includes NUL
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  fake_svc_.fake_paver().set_supported_firmware_type(type);

  ASSERT_EQ(paver_.OpenWrite(FirmwareFilename(type), size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
  EXPECT_EQ(fake_svc_.fake_paver().last_firmware_type(), type);
}

TEST_F(PaverTest, WriteFirmwareTypeTooLong) {
  const std::string type(NETBOOT_FIRMWARE_TYPE_MAX_LENGTH, 'a');  // Max length includes NUL
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);

  EXPECT_EQ(paver_.OpenWrite(FirmwareFilename(type), size, zx::duration::infinite()),
            TFTP_ERR_INVALID_ARGS);
  EXPECT_NE(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  paver_.Abort();
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));

  // Make sure the WriteFirmware() call was never made.
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(), {});
}

TEST_F(PaverTest, BootloaderUsesWriteFirmware) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_BOOTLOADER_FILENAME, size, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  ASSERT_EQ(size, sizeof(kFakeData));
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
  paver_.Close();

  // Legacy BOOTLOADER file should use WriteFirmware() FIDL with empty type.
  ValidateCommandTrace(fake_svc_.fake_paver().GetCommandTrace(),
                       {paver_test::Command::kInitializeAbr, paver_test::Command::kWriteFirmware});
  EXPECT_EQ(fake_svc_.fake_paver().last_firmware_type(), "");
}

TEST_F(PaverTest, DoubleClose) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FVM_FILENAME, size, zx::duration::infinite()), TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  paver_.Close();
  paver_.Close();
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_OK));
}

TEST_F(PaverTest, AbortFvm) {
  size_t size = sizeof(kFakeData);
  fake_svc_.fake_paver().set_expected_payload_size(size * 2);
  ASSERT_EQ(paver_.OpenWrite(NETBOOT_FVM_FILENAME, size * 2, zx::duration::infinite()),
            TFTP_NO_ERROR);
  ASSERT_EQ(paver_.Write(kFakeData, &size, 0), TFTP_NO_ERROR);
  paver_.Abort();
  ASSERT_NO_FATAL_FAILURE(AssertExitCode(ZX_ERR_CANCELED));
}

}  // namespace
