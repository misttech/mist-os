// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>

#include <fstream>
#include <iostream>

#include "src/developer/debug/zxdb/client/symbol_server_impl.h"
#include "src/developer/debug/zxdb/common/curl.h"
#include "src/developer/debug/zxdb/common/version.h"
#include "src/lib/fxl/strings/trim.h"
#include "tools/symbolizer/analytics.h"
#include "tools/symbolizer/command_line_options.h"
#include "tools/symbolizer/log_parser.h"
#include "tools/symbolizer/printer.h"
#include "tools/symbolizer/symbolizer_impl.h"

namespace symbolizer {

namespace {

// TODO(dangyi): This is a poor implementation of the authentication process. Revisit this after
// fxb/61746 is resolved.
int AuthMode() {
  debug::MessageLoopPoll loop;
  loop.Init(nullptr);

  auto server = std::make_unique<zxdb::SymbolServerImpl>(nullptr, "", true);
  if (server->state() == zxdb::SymbolServer::State::kBusy) {
    server->set_state_change_callback(
        [&](zxdb::SymbolServer*, zxdb::SymbolServer::State state) { loop.QuitNow(); });
    loop.Run();
    // Clear the callback.
    server->set_state_change_callback({});
  }
  loop.Cleanup();

  if (server->state() == zxdb::SymbolServer::State::kReady) {
    std::cout << "You have already authenticated. To use another credential, please remove "
              << "~/.fuchsia/debug/googleapi_auth and sign out gcloud using "
              << "`gcloud auth application-default revoke`\n";
    return EXIT_SUCCESS;
  }

  std::cout
      << "OOB auth workflow is deprecated (go/oauth-oob-deprecation). "
      << "To authenticate, please run the following command\n\n"
      << "  rm -f ~/.fuchsia/debug/googleapi_auth && gcloud auth application-default login\n\n"
      << "For more information, please see fxbug.dev/119250.\n";
  return EXIT_FAILURE;
}

}  // namespace

int Main(int argc, const char* argv[]) {
  using ::analytics::core_dev_tools::EarlyProcessAnalyticsOptions;

  zxdb::Curl::GlobalInit();
  auto deferred_cleanup_curl = fit::defer(zxdb::Curl::GlobalCleanup);
  auto deferred_cleanup_analytics = fit::defer(Analytics::CleanUp);
  CommandLineOptions options;

  if (const Error error = ParseCommandLine(argc, argv, &options); !error.empty()) {
    // Sometimes the error just has too many "\n" at the end.
    std::cerr << fxl::TrimString(error, "\n") << std::endl;
    return EXIT_FAILURE;
  }

  if (options.requested_version) {
    std::cout << "Version: " << zxdb::kBuildVersion << std::endl;
    return EXIT_SUCCESS;
  }

  if (EarlyProcessAnalyticsOptions<Analytics>(options.analytics, options.analytics_show)) {
    return 0;
  }
  Analytics::InitBotAware(options.analytics, false);
  Analytics::IfEnabledSendInvokeEvent();

  if (options.auth_mode) {
    return AuthMode();
  }

  Printer printer(std::cout);
  SymbolizerImpl symbolizer(&printer, options);
  LogParser parser(std::cin, &printer, &symbolizer);

  while (parser.ProcessNextLine()) {
    // until the eof in the input.
  }

  // Calling Reset at the end to make sure symbolize event is sent.
  symbolizer.Reset(false);

  return EXIT_SUCCESS;
}

}  // namespace symbolizer

int main(int argc, const char* argv[]) { return symbolizer::Main(argc, argv); }
