// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace2json/convert.h"

#include <gtest/gtest.h>
#include <rapidjson/document.h>
#include <rapidjson/istreamwrapper.h>
#include <src/lib/files/file.h>

#if defined(__APPLE__)
#include <mach-o/dyld.h>
#endif

#include <filesystem>
#include <fstream>
#include <sstream>

namespace {

using ::testing::Test;

std::string GetSelfPath() {
  std::string result;
#if defined(__APPLE__)
  // Executable path can have relative references ("..") depending on how the
  // app was launched.
  uint32_t length = 0;
  _NSGetExecutablePath(nullptr, &length);
  result.resize(length);
  _NSGetExecutablePath(&result[0], &length);
  result.resize(length - 1);  // Length included terminator.
#elif defined(__linux__)
  // The realpath() call below will resolve the symbolic link.
  result.assign("/proc/self/exe");
#else
#error Write this for your platform.
#endif

  char fullpath[PATH_MAX];
  return std::string(realpath(result.c_str(), fullpath));
}

std::string GetTestDataPath() {
  std::string path = GetSelfPath();
  size_t last_slash = path.rfind('/');
  if (last_slash == std::string::npos) {
    path = "./";
  } else {
    path.resize(last_slash + 1);
  }
  return path + "test_data/trace2json/";
}

void ConvertAndCompare(ConvertSettings settings, std::string expected_output_file) {
  ASSERT_TRUE(ConvertTrace(settings));
  std::string actual_out, expected_out;
  EXPECT_TRUE(files::ReadFileToString(settings.output_file_name, &actual_out));
  EXPECT_TRUE(files::ReadFileToString(expected_output_file, &expected_out));

  // Not using EXPECT_EQ here as the trace files can be large, so failures create an unreasonable
  // amount of error output.
  EXPECT_TRUE(actual_out == expected_out)
      << "Files " << settings.output_file_name << " and " << expected_output_file << " differ.";
}

// TODO(https://fxbug.dev/42078677): Temporarily disable this test to facilitate a roll of
// rapidjson. The latest roll contains changes to rapidjson's internal `dtoa`
// implementation, which ever so slightly changes the string representation of
// certain doubles. Once the roll goes through, we should come back, change the
// *_expected.json files with the new slightly updated double values, and
// re-enable these tests.
TEST(ConvertTest, DISABLED_SimpleTrace) {
  // simple_trace.fxt is a small hand-written trace file that exercises a few
  // basic event types (currently slice begin, slice end, slice complete, async
  // begin, and async end), and includes both inline and table referenced
  // strings. It only contains one provider.
  std::string test_data_path = GetTestDataPath();
  ConvertSettings settings;
  settings.input_file_name = test_data_path + "simple_trace.fxt";
  settings.output_file_name = test_data_path + "simple_trace_actual.json";
  ConvertAndCompare(settings, test_data_path + "simple_trace_expected.json");
}

// TODO(https://fxbug.dev/42078677): Temporarily disable this test to facilitate a roll of
// rapidjson. The latest roll contains changes to rapidjson's internal `dtoa`
// implementation, which ever so slightly changes the string representation of
// certain doubles. Once the roll goes through, we should come back, change the
// *_expected.json files with the new slightly updated double values, and
// re-enable these tests.
TEST(ConvertTest, DISABLED_ExampleBenchmark) {
  // example_benchmark.fxt is the trace written by the program in
  // garnet/examples/benchmark, in this case run on qemu. To collect the trace,
  // include //src/examples/benchmark in your build and then run:
  // ffx trace start --duration 10 --categories "benchmark"
  // ffx component run /core/ffx-laboratory:benchmark
  // "fuchsia-pkg://fuchsia.com/benchmark#meta/benchmark.cm"

  std::string test_data_path = GetTestDataPath();
  ConvertSettings settings;
  settings.input_file_name = test_data_path + "example_benchmark.fxt";
  settings.output_file_name = test_data_path + "example_benchmark_actual.json";
  ConvertAndCompare(settings, test_data_path + "example_benchmark_expected.json");
}

TEST(ConvertTest, MissingMagicNumber) {
  std::string test_data_path = GetTestDataPath();
  ConvertSettings settings;
  settings.input_file_name = test_data_path + "no_magic.fxt";
  settings.output_file_name = test_data_path + "no_magic.json";
  EXPECT_FALSE(ConvertTrace(settings));
  EXPECT_FALSE(std::filesystem::exists(settings.output_file_name));
}

}  // namespace
