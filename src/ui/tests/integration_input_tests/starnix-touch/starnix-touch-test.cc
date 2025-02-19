// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.element/cpp/fidl.h>
#include <fidl/fuchsia.input.report/cpp/fidl.h>
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
#include <lib/zx/time.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <sstream>
#include <vector>

#include <gtest/gtest.h>

#include "relay-api.h"
#include "src/ui/testing/util/portable_ui_test.h"
#include "third_party/android/platform/bionic/libc/kernel/uapi/linux/input-event-codes.h"

namespace {

// Types imported for the realm_builder library.
using component_testing::ChildRef;
using component_testing::Directory;
using component_testing::ParentRef;
using component_testing::Route;

// Alias for Component child name as provided to Realm Builder.
using ChildName = std::string;

// Maximum distance between two physical pixel coordinates so that they are considered equal.
constexpr double kEpsilon = 0.5f;

// Touch down is expressed in two `TouchEvents`s: btn_touch, Phase Add.
// Touch up is expressed in two: btn_touch, Phase Remove.
constexpr size_t kDownUpNumEvents = 4;

// Touch move is expressed in one `TouchEvents`: Phase Change.
// Touch up is expressed in two: btn_touch, Phase Remove.
constexpr size_t kMoveUpNumEvents = 3;

// Timeout for reading from touch dump socket.
constexpr zx::duration kSocketTimeout = zx::min(3);

struct TouchEvent {
  float local_x;  // The x-position, in the client's coordinate space.
  float local_y;  // The y-position, in the client's coordinate space.
  fuchsia_ui_pointer::EventPhase phase;
  int slot_id;
  int pointer_id;      // only phase add include this field.
  bool has_btn_touch;  // only btn_touch event will only have this field.
  int btn_touch;       // only btn_touch event will only have this field.
};

void ExpectLocationPhaseAndSlot(
    const TouchEvent& e, double expected_x, double expected_y,
    fuchsia_ui_pointer::EventPhase expected_phase, int expected_slot_id,
    const cpp20::source_location caller = cpp20::source_location::current()) {
  std::string caller_info = "line " + std::to_string(caller.line());
  EXPECT_EQ(expected_slot_id, e.slot_id) << " from " << caller_info;
  EXPECT_EQ(expected_phase, e.phase) << " from " << caller_info;
  EXPECT_NEAR(expected_x, e.local_x, kEpsilon) << " from " << caller_info;
  EXPECT_NEAR(expected_y, e.local_y, kEpsilon) << " from " << caller_info;
  EXPECT_EQ(false, e.has_btn_touch) << " from " << caller_info;
}

void ExpectBtnTouch(const TouchEvent& e, int expected_value,
                    const cpp20::source_location caller = cpp20::source_location::current()) {
  std::string caller_info = "line " + std::to_string(caller.line());
  EXPECT_EQ(true, e.has_btn_touch) << " from " << caller_info;
  EXPECT_EQ(expected_value, e.btn_touch) << " from " << caller_info;
}

enum class TapLocation { kTopLeft, kBottomRight };

struct StdIOSocket {
  zx::socket in_socket;
  zx::socket out_socket;
};

class StarnixTouchTest : public ui_testing::PortableUITest {
 protected:
  struct EvDevPacket {
    // The event timestamp received by Starnix, from Fuchsia.
    int64_t sec;
    int64_t usec;
    // * For an overview of the following fields, see
    //   https://kernel.org/doc/html/latest/input/input.html#event-interface
    // * For details on the constants relevant to Starnix touch input, see
    //   https://kernel.org/doc/html/latest/input/event-codes.html
    uint16_t type;
    uint16_t code;
    int32_t value;
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
  void InjectInput(TapLocation tap_location) {
    switch (tap_location) {
      case TapLocation::kTopLeft:
        InjectTap(display_width() / 4, display_height() / 4);
        break;
      case TapLocation::kBottomRight:
        InjectTap(3 * display_width() / 4, 3 * display_height() / 4);
        break;
    }
  }

  // Launches `touch_dump.cc`, connecting its `stdout` to `out_socket`.
  // Then waits for `touch_dump.cc` to report that it is ready to receive
  // input events.
  StdIOSocket LaunchDumper() {
    // Create a socket for communicating with `touch_dump`, and store it in
    // a collection of `HandleInfo`s.
    std::vector<fuchsia_process::HandleInfo> numbered_handles;
    zx::socket out_remote_socket;
    zx::socket out_socket;
    zx::socket in_remote_socket;
    zx::socket in_socket;
    zx_status_t sock_res;

    // stdout
    sock_res = zx::socket::create(ZX_SOCKET_DATAGRAM, &out_socket, &out_remote_socket);
    FX_CHECK(sock_res == ZX_OK) << "Creating socket failed: " << zx_status_get_string(sock_res);
    numbered_handles.push_back(fuchsia_process::HandleInfo{
        {.handle = zx::handle(std::move(out_remote_socket)), .id = PA_HND(PA_FD, STDOUT_FILENO)}});

    // stdin
    sock_res = zx::socket::create(ZX_SOCKET_DATAGRAM, &in_socket, &in_remote_socket);
    FX_CHECK(sock_res == ZX_OK) << "Creating socket failed: " << zx_status_get_string(sock_res);
    numbered_handles.push_back(fuchsia_process::HandleInfo{
        {.handle = zx::handle(std::move(in_remote_socket)), .id = PA_HND(PA_FD, STDIN_FILENO)}});

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

    return {.in_socket = std::move(in_socket), .out_socket = std::move(out_socket)};
  }

  // Reads sequences of touch events from `touch_dump.cc`, via `out_socket`
  // until we get num_expected events.
  //
  // Because of the variable amount of packets read at a time, we may create
  // varying  amounts of TouchEvents from a single call to GetEvDevPackets.
  // Therefore we use the running size of the final result to determine whether
  // to read more packets.
  std::vector<TouchEvent> GetTouchEventSequenceOfLen(zx::socket& out_socket, size_t num_expected) {
    std::vector<TouchEvent> result;

    while (result.size() < num_expected) {
      std::vector<EvDevPacket> pkts = GetEvDevPackets(out_socket);

      for (EvDevPacket pkt : pkts) {
        if (pkt.type == EV_SYN) {
          continue;
        }

        if (pkt.type == EV_KEY) {
          if (pkt.code != BTN_TOUCH) {
            FX_LOGS(FATAL) << "unexpected key event code in touch event seq, code=" << pkt.code;
          }
          result.push_back(TouchEvent{.has_btn_touch = true, .btn_touch = pkt.value});

          continue;
        }

        if (pkt.type != EV_ABS) {
          FX_LOGS(FATAL) << "unexpected event type in touch event seq, type=" << pkt.type;
        }

        switch (pkt.code) {
          case ABS_MT_SLOT:
            result.push_back(
                TouchEvent{.phase = fuchsia_ui_pointer::EventPhase::kChange, .slot_id = pkt.value});
            break;
          case ABS_MT_TRACKING_ID:
            if (result.empty()) {
              FX_LOGS(FATAL) << "receive ABS_MT_TRACKING_ID out of slot";
            }

            if (pkt.value == -1) {
              result[result.size() - 1].phase = fuchsia_ui_pointer::EventPhase::kRemove;
            } else {
              result[result.size() - 1].phase = fuchsia_ui_pointer::EventPhase::kAdd;
              result[result.size() - 1].pointer_id = pkt.value;
            }

            break;
          case ABS_MT_POSITION_X:
            if (result.empty()) {
              FX_LOGS(FATAL) << "receive ABS_MT_POSITION_X out of slot";
            }

            result[result.size() - 1].local_x = static_cast<float>(pkt.value);

            break;
          case ABS_MT_POSITION_Y:
            if (result.empty()) {
              FX_LOGS(FATAL) << "receive ABS_MT_POSITION_X out of slot";
            }

            result[result.size() - 1].local_y = static_cast<float>(pkt.value);

            break;
          default:
            FX_LOGS(FATAL) << "unexpected event code in touch event seq, code=" << pkt.code;
        }
      }
      FX_LOGS(INFO) << "Read " << result.size() << " events of " << num_expected;
    }

    EXPECT_EQ(result.size(), num_expected);
    return result;
  }

  void WaitForMessageFromTouchDump(zx::socket& out_socket, const std::string& message) {
    FX_LOGS(INFO) << "Waiting message " << message << " from touch_dump";
    auto packet = BlockingReadFromTouchDump(out_socket);
    ASSERT_EQ(packet, message) << "Got \"" << packet.data() << "\" with size " << packet.size();
  }

  void WriteMessageToSocket(zx::socket& in_socket, const std::string& message) {
    size_t wrote;
    in_socket.write(0, message.data(), message.size(), &wrote);
    ASSERT_EQ(wrote, message.size());
  }

 private:
  static constexpr auto kDebianRealm = "debian-realm";
  static constexpr auto kDebianRealmUrl = "#meta/debian_realm.cm";

  class RealmEventHandler : public fidl::AsyncEventHandler<fuchsia_component::Realm> {
   public:
    // Ignores any later errors on `this`. Used to avoid false-failures during
    // test teardown.
    void Stop() { running_ = false; }

    void on_fidl_error(fidl::UnbindInfo error) override {
      if (running_) {
        FX_LOGS(FATAL) << "Error on Realm client: " << error;
      }
    }

   private:
    bool running_ = true;
  };

  // To satisfy ui_testing::PortableUITest
  std::string GetTestUIStackUrl() override { return "#meta/test-ui-stack.cm"; }

  std::vector<std::pair<ChildName, std::string>> GetTestComponents() override {
    return {
        std::make_pair(kDebianRealm, kDebianRealmUrl),
    };
  }

  std::vector<Route> GetTestRoutes() override {
    return {
        // Route global capabilities from parent to the Debian realm.
        {.capabilities = {Proto<fuchsia_kernel::VmexResource>(), Proto<fuchsia_sysmem::Allocator>(),
                          Proto<fuchsia_sysmem2::Allocator>(),
                          Proto<fuchsia_tracing_provider::Registry>()},
         .source = ParentRef(),
         .targets = {ChildRef{kDebianRealm}}},

        {.capabilities =
             {
                 Directory{
                     .name = "boot-kernel",
                     .type = fuchsia::component::decl::DependencyType::STRONG,
                 },
             },
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

  // Reads a single piece of data from `touch_dump.cc`, via `out_socket`.
  //
  // There's no framing protocol between these two programs, so calling
  // code must run in lock-step with `touch_dump.cc`.
  //
  // In particular: the calling code must not send a second touch event
  // until the calling code has read the response that `touch_dump.cc`
  // sent for the first event.
  std::string BlockingReadFromTouchDump(zx::socket& out_socket) {
    std::string buf(relay_api::kMaxPacketLen * relay_api::kDownUpNumPackets, '\0');
    size_t n_read{};
    zx_status_t res{};
    zx_signals_t actual_signals;

    FX_LOGS(INFO) << "Waiting for socket to be readable";
    res = out_socket.wait_one(ZX_SOCKET_READABLE, zx::deadline_after(kSocketTimeout),
                              &actual_signals);
    FX_CHECK(res == ZX_OK) << "wait_one() returned " << zx_status_get_string(res);
    FX_CHECK(actual_signals & ZX_SOCKET_READABLE)
        << "expected signals to include ZX_SOCKET_READABLE, but actual_signals=" << actual_signals;

    res = out_socket.read(/* options = */ 0, buf.data(), buf.capacity(), &n_read);
    FX_CHECK(res == ZX_OK) << "read() returned " << zx_status_get_string(res);
    buf.resize(n_read);

    FX_CHECK(buf != relay_api::kFailedMessage);
    return buf;
  }

  std::vector<EvDevPacket> GetEvDevPackets(zx::socket& out_socket) {
    std::vector<EvDevPacket> ev_pkts;
    std::string packets = BlockingReadFromTouchDump(out_socket);
    std::size_t next = packets.find(relay_api::kEventDelimiter);
    while (next != std::string::npos) {
      packets = packets.substr(next);
      EvDevPacket ev_pkt{};
      int res = sscanf(packets.data(), relay_api::kEventFormat, &ev_pkt.sec, &ev_pkt.usec,
                       &ev_pkt.type, &ev_pkt.code, &ev_pkt.value);
      FX_CHECK(res == 5) << "Got " << res << " fields, but wanted 5";
      ev_pkts.push_back(ev_pkt);
      next = packets.find(relay_api::kEventDelimiter, relay_api::kEventDelimiter.size());
    }

    return ev_pkts;
  }

  template <typename T>
  component_testing::Protocol Proto() {
    return {fidl::DiscoverableProtocolName<T>};
  }

  // Resources for communicating with the realm server.
  // * `realm_event_handler_` must live at least as long as `realm_client_`
  // * `realm_client_` is stored in the fixture to keep `touch_dump` alive for the
  //   duration of the test
  RealmEventHandler realm_event_handler_;
  fidl::Client<fuchsia_component::Realm> realm_client_;
};

// TODO: https://fxbug.dev/42082519 - Test for DPR=2.0, too.
TEST_F(StarnixTouchTest, Tap) {
  auto [in_socket, out_socket] = LaunchDumper();

  // Wait until #launch_input is presented before injecting input.
  WaitForViewPresentation();

  // Wait for `touch_dump` to start.
  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  std::stringstream ss;
  // This test expects 2 down - up event sequences.
  ss << relay_api::kEventCmd << " " << relay_api::kDownUpNumPackets * 2;
  WriteMessageToSocket(in_socket, ss.str());

  // Wait for `touch_dump` to ready for event injection.
  WaitForMessageFromTouchDump(out_socket, relay_api::kReadyMessage);

  // Top-left.
  InjectInput(TapLocation::kTopLeft);

  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kDownUpNumEvents);
    ExpectBtnTouch(events[0], 1);
    ExpectLocationPhaseAndSlot(events[1], static_cast<float>(display_width()) / 4.f,
                               static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kAdd, 0);
    ExpectBtnTouch(events[2], 0);
    ExpectLocationPhaseAndSlot(events[3], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  // Bottom-right.
  InjectInput(TapLocation::kBottomRight);

  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kDownUpNumEvents);
    ExpectBtnTouch(events[0], 1);
    ExpectLocationPhaseAndSlot(events[1], 3 * static_cast<float>(display_width()) / 4.f,
                               3 * static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kAdd, 0);
    ExpectBtnTouch(events[2], 0);
    ExpectLocationPhaseAndSlot(events[3], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  WriteMessageToSocket(in_socket, "quit");
}

// EventsDuringFileCloseAreIgnored ensure event reader does not get events before open.
TEST_F(StarnixTouchTest, EventsDuringFileCloseAreIgnored) {
  auto [in_socket, out_socket] = LaunchDumper();

  // Wait until #launch_input is presented before injecting input.
  WaitForViewPresentation();

  // Wait for `touch_dump` to start.
  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  std::stringstream ss;
  ss << relay_api::kEventCmd << " " << relay_api::kDownUpNumPackets;
  WriteMessageToSocket(in_socket, ss.str());
  WaitForMessageFromTouchDump(out_socket, relay_api::kReadyMessage);

  // Send 1 tap to top left.
  InjectInput(TapLocation::kTopLeft);
  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kDownUpNumEvents);
    ExpectBtnTouch(events[0], 1);
    ExpectLocationPhaseAndSlot(events[1], static_cast<float>(display_width()) / 4.f,
                               static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kAdd, 0);
    ExpectBtnTouch(events[2], 0);
    ExpectLocationPhaseAndSlot(events[3], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  FX_LOGS(INFO) << "device file closed";

  // Now the file is closed. Send 1 tap to top left. touch_dump should not receive this event
  // sequence.
  InjectInput(TapLocation::kTopLeft);

  // TODO(https://fxbug.dev/375021518): Here should block on a state instead of timeout to ensure
  // events are reached to Starnix before open file below.
  // It is ok if this test is flaky when the tap top left reach to starnix after "device file
  // opened". It just means this timeout is not enough.
  RunLoopWithTimeout(zx::sec(1));

  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  WriteMessageToSocket(in_socket, ss.str());

  FX_LOGS(INFO) << "device file opened";

  // Wait for `touch_dump` to ready for event injection.
  WaitForMessageFromTouchDump(out_socket, relay_api::kReadyMessage);

  // Send 1 tap to bottom right. touch_dump should receive this event sequence.
  InjectInput(TapLocation::kBottomRight);
  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kDownUpNumEvents);
    ExpectBtnTouch(events[0], 1);
    ExpectLocationPhaseAndSlot(events[1], 3 * static_cast<float>(display_width()) / 4.f,
                               3 * static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kAdd, 0);
    ExpectBtnTouch(events[2], 0);
    ExpectLocationPhaseAndSlot(events[3], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  WriteMessageToSocket(in_socket, "quit");
}

// OpenFileDuringEventSequenceReceivesPartialSequence tests event delivery when the device file is
// opened in the middle of an event sequence. It verifies that only events generated after the file
// is opened are recorded in touch_dump.
TEST_F(StarnixTouchTest, OpenFileDuringEventSequenceReceivesPartialSequence) {
  auto [in_socket, out_socket] = LaunchDumper();

  // Wait until #launch_input is presented before injecting input.
  WaitForViewPresentation();

  // Wait for `touch_dump` to start.
  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  std::stringstream ss;
  ss << relay_api::kEventCmd << " " << relay_api::kDownUpNumPackets;
  WriteMessageToSocket(in_socket, ss.str());
  WaitForMessageFromTouchDump(out_socket, relay_api::kReadyMessage);

  // Send 1 tap to top left.
  InjectInput(TapLocation::kTopLeft);
  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kDownUpNumEvents);
    ExpectBtnTouch(events[0], 1);
    ExpectLocationPhaseAndSlot(events[1], static_cast<float>(display_width()) / 4.f,
                               static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kAdd, 0);
    ExpectBtnTouch(events[2], 0);
    ExpectLocationPhaseAndSlot(events[3], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  FX_LOGS(INFO) << "device file closed";

  // Now the file is closed. Send 1 tap to top left. touch_dump should not receive this down event.
  fuchsia_input_report::TouchInputReport down;
  down.contacts({{fuchsia_input_report::ContactInputReport{
      {
          .contact_id = 1,
          .position_x = display_width() / 4,
          .position_y = display_height() / 4,
      },
  }}});
  InjectTouchEvent(down);

  // TODO(https://fxbug.dev/375021518): Here should block on a state instead of timeout to ensure
  // events are reached to Starnix before open file below.
  // It is ok if this test is flaky when the tap top left reach to starnix after "device file
  // opened". It just means this timeout is not enough.
  RunLoopWithTimeout(zx::sec(1));

  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  // Inject touch move and up.
  ss.str("");  // clear the stringstream
  ss << relay_api::kEventCmd << " " << relay_api::kMoveNumPackets + relay_api::kUpNumPackets;
  WriteMessageToSocket(in_socket, ss.str());

  FX_LOGS(INFO) << "device file opened";

  // Wait for `touch_dump` to ready for event injection.
  WaitForMessageFromTouchDump(out_socket, relay_api::kReadyMessage);

  fuchsia_input_report::TouchInputReport move;
  move.contacts({{fuchsia_input_report::ContactInputReport{
      {
          .contact_id = 1,
          .position_x = display_width() / 4 * 3,
          .position_y = display_height() / 4 * 3,
      },
  }}});
  InjectTouchEvent(move);

  fuchsia_input_report::TouchInputReport up;
  up.contacts({{}});
  InjectTouchEvent(up);

  {
    auto events = GetTouchEventSequenceOfLen(out_socket, kMoveUpNumEvents);
    ExpectLocationPhaseAndSlot(events[0], 3 * static_cast<float>(display_width()) / 4.f,
                               3 * static_cast<float>(display_height()) / 4.f,
                               fuchsia_ui_pointer::EventPhase::kChange, 0);
    ExpectBtnTouch(events[1], 0);
    ExpectLocationPhaseAndSlot(events[2], 0.0, 0.0, fuchsia_ui_pointer::EventPhase::kRemove, 0);
  }

  WaitForMessageFromTouchDump(out_socket, relay_api::kWaitForStdinMessage);

  WriteMessageToSocket(in_socket, "quit");
}

}  // namespace
