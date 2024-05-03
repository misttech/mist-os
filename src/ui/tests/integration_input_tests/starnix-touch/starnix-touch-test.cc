// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.element/cpp/fidl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fidl/fuchsia.process/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.tracing.provider/cpp/fidl.h>
#include <fidl/fuchsia.ui.app/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.display.singleton/cpp/fidl.h>
#include <fidl/fuchsia.ui.input/cpp/fidl.h>
#include <fidl/fuchsia.ui.pointer/cpp/fidl.h>
#include <fidl/fuchsia.ui.pointer/cpp/natural_ostream.h>
#include <fidl/fuchsia.ui.test.input/cpp/fidl.h>
#include <lib/stdcompat/source_location.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "relay-api.h"
#include "src/ui/testing/util/portable_ui_test.h"
#include "third_party/android/platform/bionic/libc/kernel/uapi/linux/input-event-codes.h"

namespace {

// Types imported for the realm_builder library.
using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Route;
using component_testing::Storage;

// Alias for Component child name as provided to Realm Builder.
using ChildName = std::string;

// Maximum distance between two physical pixel coordinates so that they are considered equal.
constexpr double kEpsilon = 0.5f;

struct TouchEvent {
  float local_x;  // The x-position, in the client's coordinate space.
  float local_y;  // The y-position, in the client's coordinate space.
  fuchsia_ui_pointer::EventPhase phase;
};

void ExpectLocationAndPhase(
    const TouchEvent& e, double expected_x, double expected_y,
    fuchsia_ui_pointer::EventPhase expected_phase,
    const cpp20::source_location caller = cpp20::source_location::current()) {
  std::string caller_info = "line " + std::to_string(caller.line());
  EXPECT_NEAR(expected_x, e.local_x, kEpsilon) << " from " << caller_info;
  EXPECT_NEAR(expected_y, e.local_y, kEpsilon) << " from " << caller_info;
  EXPECT_EQ(expected_phase, e.phase) << " from " << caller_info;
}

enum class TapLocation { kTopLeft, kBottomRight };

class StarnixTouchTest : public ui_testing::PortableUITest {
 protected:
  struct EvDevPacket {
    // The event timestamp received by Starnix, from Fuchsia.
    long sec;
    long usec;
    // * For an overview of the following fields, see
    //   https://kernel.org/doc/html/latest/input/input.html#event-interface
    // * For details on the constants relevant to Starnix touch input, see
    //   https://kernel.org/doc/html/latest/input/event-codes.html
    // * Note that Starnix touch input does _not_ currently use the multi-touch protocol
    //   described in https://kernel.org/doc/html/latest/input/multi-touch-protocol.html
    unsigned short type;
    unsigned short code;
    unsigned short value;
  };

  ~StarnixTouchTest() override {
    FX_CHECK(touch_injection_request_count() > 0) << "injection expected but didn't happen.";
  }

  // To satisfy ::testing::Test
  void SetUp() override {
    ui_testing::PortableUITest::SetUp();
    FX_LOGS(INFO) << "Registering input injection device";
    RegisterTouchScreen();
  }

  // To satisfy ::testing::Test
  void TearDown() override {
    realm_event_handler_.Stop();
    ui_testing::PortableUITest::TearDown();
  }

  // For use by test cases.
  uint32_t display_width() { return display_size().width; }
  uint32_t display_height() { return display_size().height; }

  // For use by test cases.
  void InjectInput(TapLocation tap_location) {
    auto touch = std::make_unique<fuchsia_ui_input::TouchscreenReport>();
    switch (tap_location) {
      case TapLocation::kTopLeft:
        InjectTap(display_width() / 4, display_height() / 4);
        break;
      case TapLocation::kBottomRight:
        InjectTap(3 * display_width() / 4, 3 * display_height() / 4);
        break;
    }
  }

  // Launches `touch_dump.cc`, connecting its `stdout` to `local_socket_`.
  // Then waits for `touch_dump.cc` to report that it is ready to receive
  // input events.
  void LaunchDumper() {
    // Create a socket for communicating with `touch_dump`, and store it in
    // a collection of `HandleInfo`s.
    std::vector<fuchsia_process::HandleInfo> numbered_handles;
    zx::socket remote_socket;
    zx_status_t sock_res;
    sock_res = zx::socket::create(ZX_SOCKET_DATAGRAM, &local_socket_, &remote_socket);
    FX_CHECK(sock_res == ZX_OK) << "Creating socket failed: " << zx_status_get_string(sock_res);
    numbered_handles.push_back(fuchsia_process::HandleInfo{
        {.handle = zx::handle(std::move(remote_socket)), .id = PA_HND(PA_FD, STDOUT_FILENO)}});

    // Launch the child.
    FX_LOGS(INFO) << "Launching touch_dump";
    std::optional<fidl::Result<fuchsia_component::Realm::CreateChild>> create_child_status;
    zx::result<fidl::ClientEnd<fuchsia_component::Realm>> realm_proxy =
        realm_root()->component().Connect<fuchsia_component::Realm>();
    if (realm_proxy.is_error()) {
      FX_LOGS(FATAL) << "Failed to connect to Realm server: "
                     << zx_status_get_string(realm_proxy.error_value());
    }
    realm_client_ =
        fidl::Client(std::move(realm_proxy.value()), dispatcher(), &realm_event_handler_);
    realm_client_
        ->CreateChild({fuchsia_component_decl::CollectionRef(
                           {{.name = "debian_userspace"}}),  // Declared in `debian_container.cml`
                       fuchsia_component_decl::Child(
                           {{.name = "touch_dump",
                             .url = "#meta/touch_dump.cm",
                             .startup = fuchsia_component_decl::StartupMode::kLazy}}),
                       // The `ChildArgs` enable `starnix-touch-test.cc` to read from the
                       // stdout of `touch_dump.cc`.
                       fuchsia_component::CreateChildArgs(
                           {{.numbered_handles = std::move(numbered_handles)}})})
        .ThenExactlyOnce([&](auto result) { create_child_status = std::move(result); });
    RunLoopUntil([&] { return create_child_status.has_value(); });

    // Check that launching succeeded.
    const auto& status = create_child_status.value();
    FX_CHECK(!status.is_error()) << "CreateChild() returned error " << status.error_value();

    // Wait for `touch_dump` to start.
    std::string packet;
    FX_LOGS(INFO) << "Waiting for touch_dump";
    packet = BlockingReadFromTouchDump();
    FX_CHECK(packet == relay_api::kReadyMessage)
        << "Got \"" << packet.data() << "\" with size " << packet.size();
  }

  // Reads a single touch event from `touch_dump.cc`, via `local_socket_`.
  TouchEvent GetTouchEvent() {
    EvDevPacket pkt{};
    float local_x;
    float local_y;
    fuchsia_ui_pointer::EventPhase phase;

    pkt = GetEvDevPacket();
    EXPECT_EQ(pkt.type, EV_KEY);
    EXPECT_EQ(pkt.code, BTN_TOUCH);
    switch (pkt.value) {
      case 0:
        phase = fuchsia_ui_pointer::EventPhase::kRemove;
        break;
      case 1:
        phase = fuchsia_ui_pointer::EventPhase::kAdd;
        break;
      default:
        FX_LOGS(FATAL) << "Unexpected BTN_TOUCH value " << pkt.value;
        // Keep clang from getting confused about whether `phase` is initialized below.
        abort();
    }

    pkt = GetEvDevPacket();
    EXPECT_EQ(pkt.type, EV_KEY);
    EXPECT_EQ(pkt.code, BTN_TOOL_FINGER);
    switch (phase) {
      case fuchsia_ui_pointer::EventPhase::kRemove:
        EXPECT_EQ(0, pkt.value);
        break;
      case fuchsia_ui_pointer::EventPhase::kAdd:
        EXPECT_EQ(1, pkt.value);
        break;
      default:
        FX_LOGS(FATAL) << "Internal error: unexpected phase " << phase;
    }

    pkt = GetEvDevPacket();
    EXPECT_EQ(pkt.type, EV_ABS);
    EXPECT_EQ(pkt.code, ABS_X);
    local_x = pkt.value;

    pkt = GetEvDevPacket();
    EXPECT_EQ(pkt.type, EV_ABS);
    EXPECT_EQ(pkt.code, ABS_Y);
    local_y = pkt.value;

    pkt = GetEvDevPacket();
    EXPECT_EQ(pkt.type, EV_SYN);
    EXPECT_EQ(pkt.code, SYN_REPORT);

    return {.local_x = local_x, .local_y = local_y, .phase = phase};
  }

 private:
  static constexpr auto kDebianRealm = "debian-realm";
  static constexpr auto kDebianRealmUrl = "#meta/debian_realm.cm";

  class RealmEventHandler : public fidl::AsyncEventHandler<fuchsia_component::Realm> {
   public:
    RealmEventHandler() : running_(true) {}

    // Ignores any later errors on `this`. Used to avoid false-failures during
    // test teardown.
    void Stop() { running_ = false; }

    void on_fidl_error(fidl::UnbindInfo error) override {
      if (running_) {
        FX_LOGS(FATAL) << "Error on Realm client: " << error;
      }
    }

   private:
    bool running_;
  };

  // To satisfy ui_testing::PortableUITest
  std::string GetTestUIStackUrl() override { return "#meta/test-ui-stack.cm"; }

  // To satisfy ui_testing::PortableUITest
  void ExtendRealm() override {
    for (const auto& [name, component] : GetTestComponents()) {
      realm_builder().AddChild(name, component);
    }

    // Add the necessary routing for each of the extra components added above.
    for (const auto& route : GetTestRoutes()) {
      realm_builder().AddRoute(route);
    }
  }

  std::vector<std::pair<ChildName, std::string>> GetTestComponents() {
    return {
        std::make_pair(kDebianRealm, kDebianRealmUrl),
    };
  }

  std::vector<Route> GetTestRoutes() {
    return {
        // Route global capabilities from parent to the Debian realm.
        {.capabilities = {Proto<fuchsia_kernel::VmexResource>(), Proto<fuchsia_sysmem::Allocator>(),
                          Proto<fuchsia_sysmem2::Allocator>(),
                          Proto<fuchsia_tracing_provider::Registry>()},
         .source = ParentRef(),
         .targets = {ChildRef{kDebianRealm}}},

        // Route capabilities from test-ui-stack to the Debian realm.
        {.capabilities = {Proto<fuchsia_ui_composition::Allocator>(),
                          Proto<fuchsia_ui_composition::Flatland>(),
                          Proto<fuchsia_ui_display_singleton::Info>(),
                          Proto<fuchsia_element::GraphicalPresenter>()},
         .source = ui_testing::PortableUITest::kTestUIStackRef,
         .targets = {ChildRef{kDebianRealm}}},

        // Route capabilities from the Debian realm to the parent.
        {.capabilities =
             {// Allow this test to launch `touch_dump` inside the Debian realm.
              Proto<fuchsia_component::Realm>()},
         .source = ChildRef{kDebianRealm},
         .targets = {ParentRef()}},
    };
  }

  // Reads a single piece of data from `touch_dump.cc`, via `local_socket_`.
  //
  // There's no framing protocol between these two programs, so calling
  // code must run in lock-step with `touch_dump.cc`.
  //
  // In particular: the calling code must not send a second touch event
  // until the calling code has read the response that `touch_dump.cc`
  // sent for the first event.
  std::string BlockingReadFromTouchDump() {
    std::string buf(relay_api::kMaxPacketLen, '\0');
    size_t n_read{};
    zx_status_t res{};
    zx_signals_t actual_signals;

    FX_LOGS(INFO) << "Waiting for socket to be readable";
    res = local_socket_.wait_one(ZX_SOCKET_READABLE, zx::time(ZX_TIME_INFINITE), &actual_signals);
    FX_CHECK(res == ZX_OK) << "wait_one() returned " << zx_status_get_string(res);
    FX_CHECK(actual_signals & ZX_SOCKET_READABLE)
        << "expected signals to include ZX_SOCKET_READABLE, but actual_signals=" << actual_signals;

    res = local_socket_.read(/* options = */ 0, buf.data(), buf.capacity(), &n_read);
    FX_CHECK(res == ZX_OK) << "read() returned " << zx_status_get_string(res);
    buf.resize(n_read);

    FX_CHECK(buf != relay_api::kFailedMessage);
    return buf;
  }

  EvDevPacket GetEvDevPacket() {
    std::string packet = BlockingReadFromTouchDump();
    EvDevPacket ev_pkt{};
    int res = sscanf(packet.data(), relay_api::kEventFormat, &ev_pkt.sec, &ev_pkt.usec,
                     &ev_pkt.type, &ev_pkt.code, &ev_pkt.value);
    FX_CHECK(res == 5) << "Got " << res << "fields, but wanted 5";
    return ev_pkt;
  }

  template <typename T>
  component_testing::Protocol Proto() {
    return {fidl::DiscoverableProtocolName<T>};
  }

  zx::socket local_socket_;
  // Resources for communicating with the realm server.
  // * `realm_event_handler_` must live at least as long as `realm_client_`
  // * `realm_client_` is stored in the fixture to keep `touch_dump` alive for the
  //   duration of the test
  RealmEventHandler realm_event_handler_;
  fidl::Client<fuchsia_component::Realm> realm_client_;
};

// TODO: https://fxbug.dev/42082519 - Test for DPR=2.0, too.
TEST_F(StarnixTouchTest, Tap) {
  LaunchDumper();

  // Wait until #launch_input is presented before injecting input.
  WaitForViewPresentation();

  // Top-left.
  InjectInput(TapLocation::kTopLeft);
  ExpectLocationAndPhase(GetTouchEvent(), static_cast<float>(display_width()) / 4.f,
                         static_cast<float>(display_height()) / 4.f,
                         fuchsia_ui_pointer::EventPhase::kAdd);
  ExpectLocationAndPhase(GetTouchEvent(), static_cast<float>(display_width()) / 4.f,
                         static_cast<float>(display_height()) / 4.f,
                         fuchsia_ui_pointer::EventPhase::kRemove);

  // Bottom-right.
  InjectInput(TapLocation::kBottomRight);
  ExpectLocationAndPhase(GetTouchEvent(), 3 * static_cast<float>(display_width()) / 4.f,
                         3 * static_cast<float>(display_height()) / 4.f,
                         fuchsia_ui_pointer::EventPhase::kAdd);
  ExpectLocationAndPhase(GetTouchEvent(), 3 * static_cast<float>(display_width()) / 4.f,
                         3 * static_cast<float>(display_height()) / 4.f,
                         fuchsia_ui_pointer::EventPhase::kRemove);
}

}  // namespace
