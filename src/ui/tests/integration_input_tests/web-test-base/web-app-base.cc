// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/tests/integration_input_tests/web-test-base/web-app-base.h"

#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

#include <src/ui/testing/util/fidl_cpp_helpers.h>

namespace integration_tests {

namespace {

fuchsia_mem::Buffer BufferFromString(const std::string& script) {
  uint64_t num_bytes = script.size();

  zx::vmo vmo;
  FX_CHECK(zx::vmo::create(num_bytes, 0u, &vmo) == ZX_OK);
  FX_CHECK(vmo.write(script.data(), 0, num_bytes) == ZX_OK);

  return fuchsia_mem::Buffer(std::move(vmo), num_bytes);
}

std::string StringFromBuffer(const fuchsia_mem::Buffer& buffer) {
  size_t num_bytes = buffer.size();
  std::string str(num_bytes, 'x');
  buffer.vmo().read(str.data(), 0, num_bytes);
  return str;
}

}  // namespace

rapidjson::Document JsonFromBuffer(const fuchsia_mem::Buffer& buffer) {
  rapidjson::Document doc =
      json::JSONParser().ParseFromString(StringFromBuffer(buffer), "web-app-response");
  if (doc.HasParseError()) {
    FX_LOGS(FATAL) << "Failed to parse json: error code = " << doc.GetParseError();
  }

  return doc;
}

// |fuchsia_web::NavigationEventListener|
void NavListener::OnNavigationStateChanged(OnNavigationStateChangedRequest& req,
                                           OnNavigationStateChangedCompleter::Sync& completer) {
  auto& nav_state = req.change();
  if (nav_state.is_main_document_loaded().has_value()) {
    FX_LOGS(INFO) << "nav_state.is_main_document_loaded = "
                  << nav_state.is_main_document_loaded().value();
    is_main_document_loaded_ = nav_state.is_main_document_loaded().value();
  }
  if (nav_state.page_type().has_value()) {
    FX_CHECK(nav_state.page_type().value() != fuchsia_web::PageType::kError);
  }
  if (nav_state.title().has_value()) {
    title_ = nav_state.title().value();
    FX_LOGS(INFO) << "nav_state.title = " << title_;
    if (title_.find("about:blank") != std::string::npos) {
      loaded_about_blank_ = true;
    }
    if (title_.find("window_resized") != std::string::npos) {
      window_resized_ = true;
    }
  }

  completer.Reply();
}

WebAppBase::WebAppBase()
    : loop_(&kAsyncLoopConfigAttachToCurrentThread), outgoing_directory_(loop_.dispatcher()) {}

void WebAppBase::Setup(const std::string& js_code,
                       fuchsia_web::ContextFeatureFlags context_feature_flags) {
  ZX_ASSERT_OK(outgoing_directory_.ServeFromStartupInfo());
  SetupWebEngine(context_feature_flags);
  SetupViewProvider();

  FX_LOGS(INFO) << "Wait for CreateView2 called.";
  RunLoopUntil([&]() { return create_view2_called_; });
  SetupWebPage(js_code);
}

void WebAppBase::SetupWebEngine(fuchsia_web::ContextFeatureFlags context_feature_flags) {
  auto web_context_provider_connect = component::Connect<fuchsia_web::ContextProvider>();
  ZX_ASSERT_OK(web_context_provider_connect);
  web_context_provider_ = fidl::SyncClient(std::move(web_context_provider_connect.value()));

  auto service_directory = component::OpenServiceRoot();
  ZX_ASSERT_OK(service_directory);

  // Enable Vulkan to allow WebEngine run on Flatland.
  fuchsia_web::CreateContextParams params(
      {.service_directory = std::move(service_directory.value()),
       .features = context_feature_flags});

  auto [web_context_client_end, web_context_server_end] =
      fidl::Endpoints<fuchsia_web::Context>::Create();
  ZX_ASSERT_OK(
      web_context_provider_->Create({std::move(params), std::move(web_context_server_end)}));
  web_context_ = fidl::SyncClient(std::move(web_context_client_end));

  auto [web_frame_client_end, web_frame_server_end] = fidl::Endpoints<fuchsia_web::Frame>::Create();
  ZX_ASSERT_OK(web_context_->CreateFrame({std::move(web_frame_server_end)}));

  web_frame_ = fidl::SyncClient(std::move(web_frame_client_end));

  // Setup log level in JS to get logs.
  ZX_ASSERT_OK(web_frame_->SetJavaScriptLogLevel({fuchsia_web::ConsoleLogLevel::kInfo}));
}

void WebAppBase::SetupViewProvider() {
  ZX_ASSERT_OK(outgoing_directory_.AddUnmanagedProtocol<fuchsia_ui_app::ViewProvider>(
      [&](fidl::ServerEnd<fuchsia_ui_app::ViewProvider> server_end) {
        FX_LOGS(INFO) << "fuchsia_ui_app::ViewProvider connect";
        view_provider_bindings_.AddBinding(loop_.dispatcher(), std::move(server_end), this,
                                           fidl::kIgnoreBindingClosure);
      }));
}

void WebAppBase::SetupWebPage(const std::string& js_code) {
  FX_LOGS(INFO) << "Loading web app.";
  auto [navigation_event_listener_client_end, navigation_event_listener_server_end] =
      fidl::Endpoints<fuchsia_web::NavigationEventListener>::Create();
  nav_listener_bindings_.AddBinding(loop_.dispatcher(),
                                    std::move(navigation_event_listener_server_end), &nav_listener_,
                                    fidl::kIgnoreBindingClosure);
  ZX_ASSERT_OK(
      web_frame_->SetNavigationEventListener({std::move(navigation_event_listener_client_end)}));

  auto [navigation_controller_client_end, navigation_controller_server_end] =
      fidl::Endpoints<fuchsia_web::NavigationController>::Create();
  ZX_ASSERT_OK(web_frame_->GetNavigationController({std::move(navigation_controller_server_end)}));
  fidl::SyncClient navigation_controller(std::move(navigation_controller_client_end));
  ZX_ASSERT_OK(navigation_controller->LoadUrl(
      {{.url = "about:blank", .params = fuchsia_web::LoadUrlParams()}}));

  // Wait for navigation loaded "about:blank" page then inject JS code, to avoid inject JS to
  // wrong page.
  RunLoopUntil(
      [&] { return nav_listener_.loaded_about_blank_ && nav_listener_.is_main_document_loaded_; });

  ZX_ASSERT_OK(
      web_frame_->ExecuteJavaScript({{.origins = {"*"}, .script = BufferFromString(js_code)}}));

  auto [message_port_client_end, message_port_server_end] =
      fidl::Endpoints<fuchsia_web::MessagePort>::Create();
  bool is_port_registered = false;
  bool window_resized = false;
  SendMessageToWebPage(std::move(message_port_server_end), "REGISTER_PORT");

  out_message_port_ = fidl::Client(std::move(message_port_client_end), loop_.dispatcher());
  out_message_port_->ReceiveMessage().Then([&is_port_registered, &window_resized](auto& res) {
    ZX_ASSERT_OK(res);
    auto message = StringFromBuffer(res->message().data().value());
    // JS already saw window has size, don't wait for resize.
    if (message == "PORT_REGISTERED WINDOW_RESIZED") {
      window_resized = true;
    } else {
      FX_CHECK(message == "PORT_REGISTERED") << "Expected PORT_REGISTERED but got " << message;
    }
    is_port_registered = true;
  });

  FX_LOGS(INFO) << "Wait for PORT_REGISTERED";
  RunLoopUntil([&] { return is_port_registered; });

  if (!window_resized) {
    FX_LOGS(INFO) << "Wait for window resized";
    RunLoopUntil([&] { return nav_listener_.window_resized_; });
  }

  FX_LOGS(INFO) << "SetupWebPage done";
}

void WebAppBase::SendMessageToWebPage(fidl::ServerEnd<fuchsia_web::MessagePort> message_port,
                                      const std::string& message) {
  std::vector<fuchsia_web::OutgoingTransferable> outgoing;
  outgoing.emplace_back(
      fuchsia_web::OutgoingTransferable::WithMessagePort(std::move(message_port)));
  fuchsia_web::WebMessage web_message(
      {.data = BufferFromString(message), .outgoing_transfer = std::move(outgoing)});

  ZX_ASSERT_OK(web_frame_->PostMessage({/*target_origin=*/"*", std::move(web_message)}));
}

// |fuchsia_ui_app::ViewProvider|
void WebAppBase::CreateViewWithViewRef(CreateViewWithViewRefRequest& request,
                                       CreateViewWithViewRefCompleter::Sync& completer) {
  // Flatland only use |CreateView2|.
  FX_LOGS(FATAL) << "CreateViewWithViewRef() is not implemented.";
}

// |fuchsia_ui_app::ViewProvider|
void WebAppBase::CreateView2(CreateView2Request& req, CreateView2Completer::Sync& completer) {
  FX_LOGS(INFO) << "Call CreateView2";
  fuchsia_web::CreateView2Args args2(
      {.view_creation_token = std::move(req.args().view_creation_token())});
  ZX_ASSERT_OK(web_frame_->CreateView2(std::move(args2)));
  create_view2_called_ = true;
}

}  // namespace integration_tests
