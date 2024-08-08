// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.ui.app/cpp/fidl.h>
#include <fidl/fuchsia.ui.test.input/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>
#include <fidl/fuchsia.web/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <string>
#include <utility>

#include <src/lib/json_parser/json_parser.h>
#include <src/ui/testing/util/fidl_cpp_helpers.h>

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

std::vector<fuchsia_ui_test_input::MouseButton> GetPressedButtons(int buttons) {
  std::vector<fuchsia_ui_test_input::MouseButton> pressed_buttons;

  if (buttons & 0x1) {
    pressed_buttons.push_back(fuchsia_ui_test_input::MouseButton::kFirst);
  }
  if (buttons & 0x1 >> 1) {
    pressed_buttons.push_back(fuchsia_ui_test_input::MouseButton::kSecond);
  }
  if (buttons & 0x1 >> 2) {
    pressed_buttons.push_back(fuchsia_ui_test_input::MouseButton::kThird);
  }

  return pressed_buttons;
}

fuchsia_ui_test_input::MouseEventPhase GetPhase(const std::string& type) {
  if (type == "add") {
    return fuchsia_ui_test_input::MouseEventPhase::kAdd;
  }
  if (type == "hover") {
    return fuchsia_ui_test_input::MouseEventPhase::kHover;
  }
  if (type == "mousedown") {
    return fuchsia_ui_test_input::MouseEventPhase::kDown;
  }
  if (type == "mousemove") {
    return fuchsia_ui_test_input::MouseEventPhase::kMove;
  }
  if (type == "mouseup") {
    return fuchsia_ui_test_input::MouseEventPhase::kUp;
  }
  if (type == "wheel") {
    return fuchsia_ui_test_input::MouseEventPhase::kWheel;
  }
  FX_LOGS(FATAL) << "Invalid mouse event type: " << type;
  exit(1);
}

class NavListener : public fidl::Server<fuchsia_web::NavigationEventListener> {
 public:
  // |fuchsia_web::NavigationEventListener|
  void OnNavigationStateChanged(OnNavigationStateChangedRequest& req,
                                OnNavigationStateChangedCompleter::Sync& completer) override {
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
      FX_LOGS(INFO) << "nav_state.title = " << nav_state.title().value();
      if (nav_state.title() == "about:blank") {
        loaded_about_blank_ = true;
      }
      if (nav_state.title() == "window_resized") {
        window_resized_ = true;
      }
    }

    completer.Reply();
  }
  bool loaded_about_blank_ = false;
  bool is_main_document_loaded_ = false;
  bool window_resized_ = false;
};

// Implements a simple web app, which responds to mouse events.
class WebApp : public fidl::Server<fuchsia_ui_app::ViewProvider> {
 public:
  WebApp()
      : loop_(&kAsyncLoopConfigAttachToCurrentThread), outgoing_directory_(loop_.dispatcher()) {
    ZX_ASSERT_OK(outgoing_directory_.ServeFromStartupInfo());
    SetupWebEngine();
    SetupViewProvider();
  }

  void Run() {
    FX_LOGS(INFO) << "Wait for CreateView2 called.";
    RunLoopUntil([&]() { return create_view2_called_; });

    FX_LOGS(INFO) << "CreateView2 called. Loading web app.";
    NavListener navigation_event_listener;
    auto [navigation_event_listener_client_end, navigation_event_listener_server_end] =
        fidl::Endpoints<fuchsia_web::NavigationEventListener>::Create();
    fidl::BindServer(loop_.dispatcher(), std::move(navigation_event_listener_server_end),
                     &navigation_event_listener);
    ZX_ASSERT_OK(
        web_frame_->SetNavigationEventListener({std::move(navigation_event_listener_client_end)}));

    auto [navigation_controller_client_end, navigation_controller_server_end] =
        fidl::Endpoints<fuchsia_web::NavigationController>::Create();
    ZX_ASSERT_OK(
        web_frame_->GetNavigationController({std::move(navigation_controller_server_end)}));
    fidl::SyncClient navigation_controller(std::move(navigation_controller_client_end));
    ZX_ASSERT_OK(navigation_controller->LoadUrl(
        {{.url = "about:blank", .params = fuchsia_web::LoadUrlParams()}}));

    // Wait for navigation loaded "about:blank" page then inject JS code, to avoid inject JS to
    // wrong page.
    RunLoopUntil([&navigation_event_listener] {
      return navigation_event_listener.loaded_about_blank_ &&
             navigation_event_listener.is_main_document_loaded_;
    });

    ZX_ASSERT_OK(
        web_frame_->ExecuteJavaScript({{.origins = {"*"}, .script = BufferFromString(kAppCode)}}));

    auto [message_port_client_end, message_port_server_end] =
        fidl::Endpoints<fuchsia_web::MessagePort>::Create();
    bool is_port_registered = false;
    bool window_resized = false;
    SendMessageToWebPage(std::move(message_port_server_end), "REGISTER_PORT");

    fidl::Client message_port(std::move(message_port_client_end), loop_.dispatcher());
    message_port->ReceiveMessage().Then([&is_port_registered, &window_resized](auto& res) {
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
    RunLoopUntil([&] { return is_port_registered; });

    if (!window_resized) {
      RunLoopUntil(
          [&navigation_event_listener] { return navigation_event_listener.window_resized_; });
    }

    auto mouse_input_listener_connect =
        component::Connect<fuchsia_ui_test_input::MouseInputListener>();
    ZX_ASSERT_OK(mouse_input_listener_connect);
    fidl::SyncClient mouse_input_listener(std::move(mouse_input_listener_connect.value()));

    RunLoopForMouseReponse(mouse_input_listener, message_port);
  }

 private:
  static constexpr auto kAppCode = R"JS(
    let port;
    document.body.onmousemove = function(event) {
      console.assert(port != null);
      let response = JSON.stringify({
        type: event.type,
        epoch_msec: Date.now(),
        x: event.clientX,
        y: event.clientY,
        wheel_h: event.deltaX,
        wheel_v: event.deltaY,
        device_scale_factor: window.devicePixelRatio,
        buttons: event.buttons
      });
      console.info('Reporting mouse move event ', response);
      port.postMessage(response);
    };
    document.body.onmousedown = function(event) {
      console.assert(port != null);
      let response = JSON.stringify({
        type: event.type,
        epoch_msec: Date.now(),
        x: event.clientX,
        y: event.clientY,
        wheel_h: event.deltaX,
        wheel_v: event.deltaY,
        device_scale_factor: window.devicePixelRatio,
        buttons: event.buttons
      });
      console.info('Reporting mouse down event ', response);
      port.postMessage(response);
    };
    document.body.onmouseup = function(event) {
      console.assert(port != null);
      let response = JSON.stringify({
        type: event.type,
        epoch_msec: Date.now(),
        x: event.clientX,
        y: event.clientY,
        wheel_h: event.deltaX,
        wheel_v: event.deltaY,
        device_scale_factor: window.devicePixelRatio,
        buttons: event.buttons
      });
      console.info('Reporting mouse up event ', response);
      port.postMessage(response);
    };
    document.body.onwheel = function(event) {
      console.assert(port != null);
      let response = JSON.stringify({
        type: event.type,
        epoch_msec: Date.now(),
        x: event.clientX,
        y: event.clientY,
        wheel_h: event.deltaX,
        wheel_v: event.deltaY,
        device_scale_factor: window.devicePixelRatio,
        buttons: event.buttons
      });
      console.info('Reporting mouse wheel event ', response);
      port.postMessage(response);
    };
    window.onresize = function(event) {
      if (window.innerWidth != 0) {
        console.info('size: ', window.innerWidth, window.innerHeight);
        document.title = 'window_resized';
      }
    };
    function receiveMessage(event) {
      if (event.data == "REGISTER_PORT") {
        console.log("received REGISTER_PORT");
        port = event.ports[0];
        if (window.innerWidth != 0 && window.innerHeight != 0) {
          console.info('size when REGISTER_PORT: ', window.innerWidth, window.innerHeight);
          port.postMessage('PORT_REGISTERED WINDOW_RESIZED');
        } else {
          port.postMessage('PORT_REGISTERED');
        }
      } else {
        console.error('received unexpected message: ' + event.data);
      }
    };
    window.addEventListener('message', receiveMessage, false);
    console.info('JS loaded');
  )JS";

  void SetupWebEngine() {
    auto web_context_provider_connect = component::Connect<fuchsia_web::ContextProvider>();
    ZX_ASSERT_OK(web_context_provider_connect);
    web_context_provider_ = fidl::SyncClient(std::move(web_context_provider_connect.value()));

    auto service_directory = component::OpenServiceRoot();
    ZX_ASSERT_OK(service_directory);

    // Enable Vulkan to allow WebEngine run on Flatland.
    fuchsia_web::CreateContextParams params(
        {.service_directory = std::move(service_directory.value()),
         .features = fuchsia_web::ContextFeatureFlags::kVulkan |
                     fuchsia_web::ContextFeatureFlags::kNetwork});

    auto [web_context_client_end, web_context_server_end] =
        fidl::Endpoints<fuchsia_web::Context>::Create();
    ZX_ASSERT_OK(
        web_context_provider_->Create({std::move(params), std::move(web_context_server_end)}));
    web_context_ = fidl::SyncClient(std::move(web_context_client_end));

    auto [web_frame_client_end, web_frame_server_end] =
        fidl::Endpoints<fuchsia_web::Frame>::Create();
    ZX_ASSERT_OK(web_context_->CreateFrame({std::move(web_frame_server_end)}));

    web_frame_ = fidl::SyncClient(std::move(web_frame_client_end));

    // Setup log level in JS to get logs.
    ZX_ASSERT_OK(web_frame_->SetJavaScriptLogLevel({fuchsia_web::ConsoleLogLevel::kInfo}));
  }

  void SetupViewProvider() {
    ZX_ASSERT_OK(outgoing_directory_.AddUnmanagedProtocol<fuchsia_ui_app::ViewProvider>(
        [&](fidl::ServerEnd<fuchsia_ui_app::ViewProvider> server_end) {
          FX_LOGS(INFO) << "fuchsia_ui_app::ViewProvider connect";
          fidl::BindServer(loop_.dispatcher(), std::move(server_end), this);
        }));
  }

  // |fuchsia::ui::app::ViewProvider|
  void CreateViewWithViewRef(CreateViewWithViewRefRequest& request,
                             CreateViewWithViewRefCompleter::Sync& completer) override {
    // Flatland only use |CreateView2|.
    FX_LOGS(FATAL) << "CreateViewWithViewRef() is not implemented.";
  }

  // |fuchsia_ui_app::ViewProvider|
  void CreateView2(CreateView2Request& req, CreateView2Completer::Sync& completer) override {
    FX_LOGS(INFO) << "Call CreateView2";
    fuchsia_web::CreateView2Args args2(
        {.view_creation_token = std::move(req.args().view_creation_token())});
    ZX_ASSERT_OK(web_frame_->CreateView2(std::move(args2)));
    create_view2_called_ = true;
  }

  void SendMessageToWebPage(fidl::ServerEnd<fuchsia_web::MessagePort> message_port,
                            const std::string& message) {
    std::vector<fuchsia_web::OutgoingTransferable> outgoing;
    outgoing.emplace_back(
        fuchsia_web::OutgoingTransferable::WithMessagePort(std::move(message_port)));
    fuchsia_web::WebMessage web_message(
        {.data = BufferFromString(message), .outgoing_transfer = std::move(outgoing)});

    ZX_ASSERT_OK(web_frame_->PostMessage({/*target_origin=*/"*", std::move(web_message)}));
  }

  template <typename PredicateT>
  void RunLoopUntil(PredicateT predicate) {
    while (!predicate()) {
      loop_.Run(zx::time::infinite(), true);
    }
  }

  void RunLoopForMouseReponse(
      fidl::SyncClient<fuchsia_ui_test_input::MouseInputListener>& mouse_input_listener,
      fidl::Client<fuchsia_web::MessagePort>& message_port) {
    while (true) {
      bool got_mouse_event = false;
      FX_LOGS(INFO) << "Waiting for mouse response message";

      message_port->ReceiveMessage().Then([&mouse_input_listener, &got_mouse_event](auto& res) {
        ZX_ASSERT_OK(res);

        std::optional<rapidjson::Document> mouse_response;
        mouse_response = json::JSONParser().ParseFromString(
            StringFromBuffer(res->message().data().value()), "web-app-response");
        // Validate structure of mouse response.
        const auto& mouse_resp = mouse_response.value();
        FX_CHECK(mouse_resp.HasMember("type"));
        FX_CHECK(mouse_resp.HasMember("epoch_msec"));
        FX_CHECK(mouse_resp.HasMember("x"));
        FX_CHECK(mouse_resp.HasMember("y"));
        FX_CHECK(mouse_resp.HasMember("device_scale_factor"));
        FX_CHECK(mouse_resp.HasMember("buttons"));
        FX_CHECK(mouse_resp["type"].IsString());
        FX_CHECK(mouse_resp["epoch_msec"].IsInt64());
        FX_CHECK(mouse_resp["x"].IsNumber());
        FX_CHECK(mouse_resp["y"].IsNumber());
        FX_CHECK(mouse_resp["device_scale_factor"].IsNumber());
        FX_CHECK(mouse_resp["buttons"].IsNumber());

        // Relay response to parent.
        fuchsia_ui_test_input::MouseInputListenerReportMouseInputRequest request({
            .local_x = mouse_resp["x"].GetDouble(),
            .local_y = mouse_resp["y"].GetDouble(),
            .time_received = mouse_resp["epoch_msec"].GetInt64() * 1000 * 1000,
            .component_name = "mouse-input-chromium",
            .buttons = GetPressedButtons(mouse_resp["buttons"].GetInt()),
            .phase = GetPhase(mouse_resp["type"].GetString()),
            .device_pixel_ratio = mouse_resp["device_scale_factor"].GetDouble(),
        });

        if (mouse_resp.HasMember("wheel_h")) {
          FX_CHECK(mouse_resp["wheel_h"].IsNumber());
          request.wheel_x_physical_pixel() = mouse_resp["wheel_h"].GetDouble();
        }
        if (mouse_resp.HasMember("wheel_v")) {
          FX_CHECK(mouse_resp["wheel_v"].IsNumber());
          request.wheel_y_physical_pixel() = mouse_resp["wheel_v"].GetDouble();
        }

        FX_LOGS(INFO) << "Got mouse response message " << mouse_resp["type"].GetString();

        ZX_ASSERT_OK(mouse_input_listener->ReportMouseInput(request));

        got_mouse_event = true;
      });
      RunLoopUntil([&]() { return got_mouse_event; });
    }
  }

  fidl::ProtocolHandler<fuchsia_ui_app::ViewProvider> GetHandler() {
    return view_provider_bindings_.CreateHandler(this, loop_.dispatcher(),
                                                 fidl::kIgnoreBindingClosure);
  }

  async::Loop loop_;
  component::OutgoingDirectory outgoing_directory_;
  fidl::ServerBindingGroup<fuchsia_ui_app::ViewProvider> view_provider_bindings_;
  fidl::SyncClient<fuchsia_web::ContextProvider> web_context_provider_;
  fidl::SyncClient<fuchsia_web::Context> web_context_;
  fidl::SyncClient<fuchsia_web::Frame> web_frame_;
  bool create_view2_called_ = false;
};
}  // namespace

int main(int argc, const char** argv) { WebApp().Run(); }
