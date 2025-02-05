// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/startup_annotations.h"

#include <lib/syslog/cpp/macros.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/feedback/constants.h"
#include "src/developer/forensics/feedback/reboot_log/annotations.h"
#include "src/developer/forensics/testing/gmatchers.h"
#include "src/developer/forensics/testing/gpretty_printers.h"
#include "src/developer/forensics/testing/scoped_memfs_manager.h"
#include "src/lib/files/file.h"

namespace forensics::feedback {
namespace {

using ::testing::_;
using ::testing::Key;
using ::testing::Pair;
using ::testing::UnorderedElementsAre;
using ::testing::UnorderedElementsAreArray;

class StartupAnnotationsTest : public ::testing::Test {
 public:
  void SetUp() override {}

  void TearDown() override {}

  void WriteFile(const std::string& path, const std::string& data) {
    FX_CHECK(files::WriteFile(path, data)) << "Failed to write to " << path;
  }

  void WriteFiles(const std::map<std::string, std::string>& paths_and_data) {
    for (const auto& [path, data] : paths_and_data) {
      WriteFile(path, data);
    }
  }
};

TEST_F(StartupAnnotationsTest, Keys) {
  const RebootLog reboot_log(RebootReason::kOOM, "", /*last_boot_uptime=*/std::nullopt,
                             /*last_boot_runtime=*/std::nullopt, /*critical_process=*/std::nullopt);
  const auto startup_annotations = GetStartupAnnotations(reboot_log);

  EXPECT_THAT(startup_annotations, UnorderedElementsAreArray({
                                       Key(kBuildBoardKey),
                                       Key(kBuildProductKey),
                                       Key(kBuildLatestCommitDateKey),
                                       Key(kBuildVersionKey),
                                       Key(kBuildVersionPreviousBootKey),
                                       Key(kBuildPlatformVersionKey),
                                       Key(kBuildPlatformVersionPreviousBootKey),
                                       Key(kBuildProductVersionKey),
                                       Key(kBuildProductVersionPreviousBootKey),
                                       Key(kBuildIsDebugKey),
                                       Key(kDeviceBoardNameKey),
                                       Key(kDeviceNumCPUsKey),
                                       Key(kSystemBootIdCurrentKey),
                                       Key(kSystemBootIdPreviousKey),
                                       Key(kSystemLastRebootReasonKey),
                                       Key(kSystemLastRebootUptimeKey),
                                   }));
}

TEST_F(StartupAnnotationsTest, Values_FilesPresent) {
  testing::ScopedMemFsManager memfs_manager;

  memfs_manager.Create("/config/build-info");
  memfs_manager.Create("/cache");
  memfs_manager.Create("/data");
  memfs_manager.Create("/tmp");

  WriteFiles({
      {kBuildBoardPath, "board"},
      {kBuildProductPath, "product"},
      {kBuildCommitDatePath, "commit-date"},
      {kCurrentBuildVersionPath, "current-version"},
      {kPreviousBuildVersionPath, "previous-version"},
      {kCurrentBuildPlatformVersionPath, "current-platform-version"},
      {kPreviousBuildPlatformVersionPath, "previous-platform-version"},
      {kCurrentBuildProductVersionPath, "current-product-version"},
      {kPreviousBuildProductVersionPath, "previous-product-version"},
      {kCurrentBootIdPath, "current-boot-id"},
      {kPreviousBootIdPath, "previous-boot-id"},
  });

  const RebootLog reboot_log(RebootReason::kOOM, "", /*last_boot_uptime=*/std::nullopt,
                             /*last_boot_runtime=*/std::nullopt, /*critical_process=*/std::nullopt);
  const auto startup_annotations = GetStartupAnnotations(reboot_log);

  EXPECT_THAT(
      startup_annotations,
      UnorderedElementsAre(
          Pair(kBuildBoardKey, ErrorOrString("board")),
          Pair(kBuildProductKey, ErrorOrString("product")),
          Pair(kBuildLatestCommitDateKey, ErrorOrString("commit-date")),
          Pair(kBuildVersionKey, ErrorOrString("current-version")),
          Pair(kBuildVersionPreviousBootKey, ErrorOrString("previous-version")),
          Pair(kBuildPlatformVersionKey, ErrorOrString("current-platform-version")),
          Pair(kBuildPlatformVersionPreviousBootKey, ErrorOrString("previous-platform-version")),
          Pair(kBuildProductVersionKey, ErrorOrString("current-product-version")),
          Pair(kBuildProductVersionPreviousBootKey, ErrorOrString("previous-product-version")),
          Pair(kBuildIsDebugKey, _), Pair(kDeviceBoardNameKey, _), Pair(kDeviceNumCPUsKey, _),
          Pair(kSystemBootIdCurrentKey, ErrorOrString("current-boot-id")),
          Pair(kSystemBootIdPreviousKey, ErrorOrString("previous-boot-id")),
          Pair(kSystemLastRebootReasonKey, ErrorOrString(LastRebootReasonAnnotation(reboot_log))),
          Pair(kSystemLastRebootUptimeKey, LastRebootUptimeAnnotation(reboot_log))));
}

TEST_F(StartupAnnotationsTest, Values_FilesMissing) {
  const RebootLog reboot_log(RebootReason::kOOM, "", /*last_boot_uptime=*/std::nullopt,
                             /*last_boot_runtime=*/std::nullopt, /*critical_process=*/std::nullopt);
  const auto startup_annotations = GetStartupAnnotations(reboot_log);

  EXPECT_THAT(
      startup_annotations,
      UnorderedElementsAre(
          Pair(kBuildBoardKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildProductKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildLatestCommitDateKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildVersionKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildVersionPreviousBootKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildPlatformVersionKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildPlatformVersionPreviousBootKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildProductVersionKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildProductVersionPreviousBootKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildIsDebugKey, _), Pair(kDeviceBoardNameKey, _), Pair(kDeviceNumCPUsKey, _),
          Pair(kSystemBootIdCurrentKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kSystemBootIdPreviousKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kSystemLastRebootReasonKey, ErrorOrString(LastRebootReasonAnnotation(reboot_log))),
          Pair(kSystemLastRebootUptimeKey, LastRebootUptimeAnnotation(reboot_log))));
}

TEST_F(StartupAnnotationsTest, BuildProductVersionPreviousBootFallback) {
  testing::ScopedMemFsManager memfs_manager;
  memfs_manager.Create("/data");
  memfs_manager.Create("/tmp");

  // On the first OTA, the build platform and product versions for the previous boot won't be
  // available. The build product version should match the legacy build version.
  WriteFiles({
      {kCurrentBuildVersionPath, "current-version"},
      {kPreviousBuildVersionPath, "previous-version"},
      {kCurrentBuildPlatformVersionPath, "current-platform-version"},
      {kCurrentBuildProductVersionPath, "current-product-version"},
  });

  const RebootLog reboot_log(RebootReason::kOOM, "", /*last_boot_uptime=*/std::nullopt,
                             /*last_boot_runtime=*/std::nullopt, /*critical_process=*/std::nullopt);
  const auto startup_annotations = GetStartupAnnotations(reboot_log);

  EXPECT_THAT(
      startup_annotations,
      UnorderedElementsAre(
          Pair(kBuildBoardKey, _), Pair(kBuildProductKey, _), Pair(kBuildLatestCommitDateKey, _),
          Pair(kBuildVersionKey, ErrorOrString("current-version")),
          Pair(kBuildVersionPreviousBootKey, ErrorOrString("previous-version")),
          Pair(kBuildPlatformVersionKey, ErrorOrString("current-platform-version")),
          Pair(kBuildPlatformVersionPreviousBootKey, ErrorOrString(Error::kFileReadFailure)),
          Pair(kBuildProductVersionKey, ErrorOrString("current-product-version")),
          Pair(kBuildProductVersionPreviousBootKey, ErrorOrString("previous-version")),
          Pair(kBuildIsDebugKey, _), Pair(kDeviceBoardNameKey, _), Pair(kDeviceNumCPUsKey, _),
          Pair(kSystemBootIdCurrentKey, _), Pair(kSystemBootIdPreviousKey, _),
          Pair(kSystemLastRebootReasonKey, _), Pair(kSystemLastRebootUptimeKey, _)));
}

}  // namespace
}  // namespace forensics::feedback
