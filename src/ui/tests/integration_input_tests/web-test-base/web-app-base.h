// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_WEB_TEST_BASE_WEB_APP_BASE_H_
#define SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_WEB_TEST_BASE_WEB_APP_BASE_H_

#include <fidl/fuchsia.ui.app/cpp/fidl.h>
#include <fidl/fuchsia.web/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/channel.h>

#include <string>

#include <src/lib/json_parser/json_parser.h>

namespace integration_tests {

// Parse buffer to json object. Exception when parsing failed.
rapidjson::Document JsonFromBuffer(const fuchsia_mem::Buffer& buffer);

// NavListener watches webpage loading status.
class NavListener : public fidl::Server<fuchsia_web::NavigationEventListener> {
 public:
  // |fuchsia_web::NavigationEventListener|
  void OnNavigationStateChanged(OnNavigationStateChangedRequest& req,
                                OnNavigationStateChangedCompleter::Sync& completer) override;

  bool is_main_document_loaded_ = false;
  bool loaded_about_blank_ = false;
  bool window_resized_ = false;
  std::string title_;
};

// This base class serves as a bridge between the integration test and a web
// application running within a Fuchsia web frame. It allows sub-class to
// inject JavaScript, and use message port communication between this class and
// the web app.
class WebAppBase : public fidl::Server<fuchsia_ui_app::ViewProvider> {
 public:
  WebAppBase();

  // |fuchsia_ui_app::ViewProvider|
  void CreateViewWithViewRef(CreateViewWithViewRefRequest& request,
                             CreateViewWithViewRefCompleter::Sync& completer) override;
  // |fuchsia_ui_app::ViewProvider|
  void CreateView2(CreateView2Request& req, CreateView2Completer::Sync& completer) override;

 protected:
  // Parameters:
  // - `js_code`: injected to web page. It requires REGISTER_PORT/
  //    WINDOW_RESIZED handling in receiveMessage(), see mouse-input-chromium
  //    for example.
  void Setup(const std::string& js_code, fuchsia_web::ContextFeatureFlags context_feature_flags =
                                             fuchsia_web::ContextFeatureFlags::kVulkan |
                                             fuchsia_web::ContextFeatureFlags::kNetwork);

  template <typename PredicateT>
  void RunLoopUntil(PredicateT predicate) {
    while (!predicate()) {
      loop_.Run(zx::time::infinite(), true);
    }
  }

  NavListener nav_listener_;
  // message_port used for JS send message to this class.
  fidl::Client<fuchsia_web::MessagePort> out_message_port_;

 private:
  void SetupWebEngine(fuchsia_web::ContextFeatureFlags context_feature_flags);
  void SetupViewProvider();
  void SetupWebPage(const std::string& js_code);
  void SendMessageToWebPage(fidl::ServerEnd<fuchsia_web::MessagePort> message_port,
                            const std::string& message);

  async::Loop loop_;
  component::OutgoingDirectory outgoing_directory_;
  fidl::ServerBindingGroup<fuchsia_web::NavigationEventListener> nav_listener_bindings_;
  fidl::ServerBindingGroup<fuchsia_ui_app::ViewProvider> view_provider_bindings_;
  fidl::SyncClient<fuchsia_web::ContextProvider> web_context_provider_;
  fidl::SyncClient<fuchsia_web::Context> web_context_;
  fidl::SyncClient<fuchsia_web::Frame> web_frame_;
  bool create_view2_called_ = false;
};

}  // namespace integration_tests

#endif  // SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_WEB_TEST_BASE_WEB_APP_BASE_H_
