// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/shell/josh/console/console.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <stdlib.h>

#include <array>
#include <fstream>
#include <future>
#include <string>

#include <gtest/gtest.h>

#include "src/storage/memfs/mounted_memfs.h"

namespace shell {

class ConsoleTest : public ::testing::Test {
 public:
  // Generate a random file. The last six characters of the name template must be XXXXXX
  std::string GetRandomFile(const char *name_template) {
    std::unique_ptr<char[]> buffer(new char[strlen(name_template) + 1]);
    strcpy(buffer.get(), name_template);
    mkstemp(buffer.get());
    return buffer.get();
  }

 protected:
  void SetUp() override {
    ASSERT_EQ(loop_.StartThread(), ZX_OK);
    zx::result memfs = MountedMemfs::Create(loop_.dispatcher(), "/test_tmp");
    ASSERT_TRUE(memfs.is_ok()) << memfs.status_string();
    memfs_.emplace(std::move(memfs.value()));

    // Make sure file creation is OK so memfs is running OK.
    char tmpfs_test_file[] = "/test_tmp/write.test.XXXXXX";
    ASSERT_NE(mkstemp(tmpfs_test_file), -1);
  }

  void TearDown() override {
    // Synchronously clean up.
    if (std::optional memfs = std::exchange(memfs_, std::nullopt); memfs.has_value()) {
      std::promise<zx_status_t> promise;
      memfs.value()->Shutdown([&promise](zx_status_t status) { promise.set_value(status); });
      ASSERT_EQ(promise.get_future().get(), ZX_OK);
    }

    loop_.Shutdown();
  }

  void AssertJsArgsEq(int argc, const char **argv, std::string_view expect_list) {
    // Test script
    std::string random_script = GetRandomFile("/test_tmp/script.js.XXXXXX");
    std::ofstream test_script;
    test_script.open(random_script);
    test_script << "var expectArgs = [\n"
                << expect_list << "\n];\n"
                << R"(
      if (scriptArgs.length != expectArgs.length)
        throw `*** [Error] Failed parsing all script args, expecting ${expectArgs.length}, got ${scriptArgs.length}`;
      for (i = 0; i < expectArgs.length; i++) {
        if (scriptArgs[i] != expectArgs[i])
          throw `*** [Error] Failed to parse all script arg[0], expecting "${expectArgs[i]}", got "${scriptArgs[i]}"`;
      }
    )";
    test_script.close();

    int real_argc = argc + 3;
    const char *real_argv[real_argc];
    // Will run above test script
    real_argv[0] = "test_program";
    real_argv[1] = "-r";
    real_argv[2] = random_script.c_str();
    for (int i = 0; i < argc; i++) {
      real_argv[i + 3] = argv[i];
    }

    ASSERT_EQ(0, shell::ConsoleMain(real_argc, real_argv));
  }

  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::optional<MountedMemfs> memfs_;
};

// Sanity check test to make sure Hello World works.
TEST_F(ConsoleTest, Sanity) {
  std::string filename = GetRandomFile("/test_tmp/tmp.XXXXXX");

  // Generate the js command to run.
  std::string command;
  std::string expected = "Hello World";

  // Write out the test file
  command += "file = std.open('" + filename + "', 'rw+');\n";
  command += "file.puts('" + expected + "');\n";
  command += "file.flush();\n";

  std::array argv{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-c",
      command.c_str(),
      "--",
      // The following are addition parameters to JS env
      "PARAM1=AAA",
      "PARAM2=BBB",
      "PARAM3=CCC",
  };
  ASSERT_EQ(0, shell::ConsoleMain(argv.size(), argv.data()));

  std::ifstream in(filename.c_str());
  std::string actual((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
  ASSERT_STREQ(expected.c_str(), actual.c_str());
}

// Sanity check test to make sure Hello World script works.
TEST_F(ConsoleTest, ScriptSanity) {
  std::string random_filename = GetRandomFile("/test_tmp/tmp.XXXXXX");
  std::string random_script_name = GetRandomFile("/test_tmp/script.js.XXXXXX");

  // Write js into the script file
  std::string expected = "Hello World";
  std::ofstream test_script;
  test_script.open(random_script_name);

  // Write out the test file
  test_script << "file = std.open('" + random_filename + "', 'rw+');\n";
  test_script << "file.puts('" + expected + "');\n";
  test_script << "file.flush();\n";
  test_script.close();

  std::array argv{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-r",
      random_script_name.c_str(),
      "--",
      // The following are addition parameters to JS script
      "PARAM1=AAA",
      "PARAM2=BBB",
      "PARAM3=CCC",
  };
  ASSERT_EQ(0, shell::ConsoleMain(argv.size(), argv.data()));

  std::ifstream in(random_filename.c_str());
  std::string actual((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
  ASSERT_STREQ(expected.c_str(), actual.c_str());
}

TEST_F(ConsoleTest, JsScriptArgsParsing) {
  // No double dash
  // josh --foo A B C => []
  std::array argv_no_dash{
      // The following are addition parameters to JS script
      "AAA",
      "BBB",
      "CCC",
  };
  AssertJsArgsEq(argv_no_dash.size(), argv_no_dash.data(), R"()");

  // All remaining params are js args
  // josh --foo -- A B C => [A, B, C]
  std::array argv_standard_dash{
      "--",
      // The following are addition parameters to JS script
      "AAA",
      "BBB",
      "CCC",
  };
  AssertJsArgsEq(argv_standard_dash.size(), argv_standard_dash.data(), R"(
    "AAA",
    "BBB",
    "CCC",
  )");

  // Some remaining params are js args
  // josh --foo A -- B C => [B, C]
  std::array argv1{
      "AAA",
      "--",
      // The following are addition parameters to JS script
      "BBB",
      "CCC",
  };
  AssertJsArgsEq(argv1.size(), argv1.data(), R"(
    "BBB",
    "CCC",
  )");

  // Some remaining params are js args
  // josh --foo -- A -- B C => [A, --, B, C]
  std::array argv2{
      "--",
      "AAA",
      "--",
      // The following are addition parameters to JS script
      "BBB",
      "CCC",
  };
  AssertJsArgsEq(argv2.size(), argv2.data(), R"(
    "AAA",
    "--",
    "BBB",
    "CCC",
  )");

  // No remaining params is js arg
  // josh --foo A B C -- => []
  std::array argv3{
      "AAA", "BBB", "CCC", "--",
      // The following (none) are addition parameters to JS script
  };
  AssertJsArgsEq(argv3.size(), argv3.data(), R"()");

  // No remaining params is js arg
  // josh --foo -- A B C -- => [A, B, C, --]
  std::array argv4{
      "--", "AAA", "BBB", "CCC", "--",
      // The following (none) are addition parameters to JS script
  };
  AssertJsArgsEq(argv4.size(), argv4.data(), R"(
    "AAA",
    "BBB",
    "CCC",
    "--",
  )");
}

TEST_F(ConsoleTest, CatchError) {
  // Test script - unhandled error
  std::string random_script_unhandled = GetRandomFile("/test_tmp/script.js.XXXXXX");
  std::ofstream test_script_unhandled;
  test_script_unhandled.open(random_script_unhandled);
  test_script_unhandled << R"(
    function throw_error() {
      throw "This is an unhandled error!";
    }

    throw_error();
  )";
  test_script_unhandled.close();

  std::array argv_unhandled{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-r",
      random_script_unhandled.c_str(),
      "--",
  };
  ASSERT_EQ(1, shell::ConsoleMain(argv_unhandled.size(), argv_unhandled.data()));

  // Test script - handled error
  std::string random_script_handled = GetRandomFile("/test_tmp/script.js.XXXXXX");
  std::ofstream test_script_handled;
  test_script_handled.open(random_script_handled);
  test_script_handled << R"(
    function throw_error() {
      throw "This is a handled error!";
    }

    try {
      throw_error();
    } catch (e) {
      // Do nothing
    }
  )";
  test_script_handled.close();

  std::array argv_handled{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-r",
      random_script_handled.c_str(),
      "--",
  };
  ASSERT_EQ(0, shell::ConsoleMain(argv_handled.size(), argv_handled.data()));
}

// Sanity check test to make sure async errors are caught.
TEST_F(ConsoleTest, CatchErrorAsync) {
  // Test script - unhandled error
  std::string random_script_unhandled = GetRandomFile("/test_tmp/script.js.XXXXXX");
  std::ofstream test_script_unhandled;
  test_script_unhandled.open(random_script_unhandled);
  test_script_unhandled << R"(
    async function throw_async_error() {
      throw "This is an unhandled async error!";
    }

    throw_async_error();
  )";
  test_script_unhandled.close();

  std::array argv_unhandled{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-r",
      random_script_unhandled.c_str(),
      "--",
  };
  ASSERT_EQ(1, shell::ConsoleMain(argv_unhandled.size(), argv_unhandled.data()));

  // Test script - handled error
  std::string random_script_handled = GetRandomFile("/test_tmp/script.js.XXXXXX");
  std::ofstream test_script_handled;
  test_script_handled.open(random_script_handled);
  test_script_handled << R"(
    async function throw_async_error() {
      try {
        throw "This is a handled async error!";
      } catch (e) {
        // Do nothing
      }
    }

    throw_async_error();
  )";
  test_script_handled.close();

  std::array argv_handled{
      "test_program",
      "-j",
      "/pkg/data/lib/",
      "-f",
      "/pkg/data/fidling",
      "-r",
      random_script_handled.c_str(),
      "--",
  };
  ASSERT_EQ(0, shell::ConsoleMain(argv_handled.size(), argv_handled.data()));
}

}  // namespace shell
