// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_FS_TEST_FS_TEST_FIXTURE_H_
#define SRC_STORAGE_FS_TEST_FS_TEST_FIXTURE_H_

#include <zircon/compiler.h>

#include <string>
#include <string_view>

#include <gtest/gtest.h>

#include "src/storage/fs_test/fs_test.h"
#include "src/storage/fs_test/test_filesystem.h"

namespace fs_test {

struct PowerCutOptions {
  // If true, reformat after each iteration.
  bool reformat = false;

  // The number of blocks to increment after each iteration.
  int stride = 1;
};

class BaseFilesystemTest : public testing::Test {
 public:
  explicit BaseFilesystemTest(const TestFilesystemOptions& options)
      : fs_([&options]() {
          zx::result result = TestFilesystem::Create(options);
          ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());
          return std::move(result.value());
        }()) {}
  ~BaseFilesystemTest() override;

  std::string GetPath(std::string_view relative_path) const {
    std::string path = fs_.mount_path();
    path.append(relative_path);
    return path;
  }

  TestFilesystem& fs() { return fs_; }
  const TestFilesystem& fs() const { return fs_; }

  // Repeatedly run the given test function simulating a power cut at different block write counts
  // for each iteration.
  void RunSimulatedPowerCutTest(const PowerCutOptions& options,
                                const std::function<void()>& test_function);

 protected:
  // RAII helper to swap the UTC reference clock with a continuous monotonic clock.  The fake clock
  // starts at the backstop time (typically time of latest commit), providing a plausible source.
  // We have to swap clocks before starting the filesystem, as it will inherit the UTC clock from
  // this process via the PA_CLOCK_UTC handle.
  //
  // TODO(b/295537827): Investigate if we still need to do this once the system UTC clock starts
  // automatically. We want to try and use the real UTC time source where possible.
  class ClockSwapper {
   public:
    // Create and replace the process-global UTC clock with a monotonic equivalent.
    ClockSwapper();
    // Restore the original UTC clock.
    ~ClockSwapper();

   private:
    zx_handle_t utc_clock_;  // Original UTC clock this process inherited.
  } swapper_;

  TestFilesystem fs_;
};

// Example:
//
// #include "fs_test_fixture.h"
//
// using MyTest = FilesystemTest;
//
// TEST_P(MyTest, CheckThatFooSucceeds) {
//   ...
// }
//
// INSTANTIATE_TEST_SUITE_P(FooTests, MyTest,
//                          testing::ValuesIn(AllTestFilesystems()),
//                          testing::PrintToStringParamName());

class FilesystemTest : public BaseFilesystemTest,
                       public testing::WithParamInterface<TestFilesystemOptions> {
 protected:
  FilesystemTest() : BaseFilesystemTest(GetParam()) {}
};

}  // namespace fs_test

#endif  // SRC_STORAGE_FS_TEST_FS_TEST_FIXTURE_H_
