// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/tests/basic_test.h"

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fdio/fdio.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>

#include <cmath>
#include <optional>
#include <string>

#include <gtest/gtest.h>

#include "src/media/audio/drivers/tests/test_base.h"

namespace media::audio::drivers::test {

namespace {

constexpr bool kLogGainValues = false;
void LogGainState(std::string_view prologue,
                  const fuchsia::hardware::audio::GainState& gain_state) {
  if constexpr (kLogGainValues) {
    const char* mute_state;
    const char* agc_state;
    if (gain_state.has_muted()) {
      mute_state = gain_state.muted() ? "true" : "false";
    } else {
      mute_state = "UNSPECIFIED";
    }
    if (gain_state.has_muted()) {
      agc_state = gain_state.has_agc_enabled() ? "enabled" : "disabled";
    } else {
      agc_state = "UNSPECIFIED";
    }
    FX_LOGS(INFO) << prologue
                  << (gain_state.has_gain_db() ? std::to_string(gain_state.gain_db())
                                               : "UNSPECIFIED")
                  << " dB, muted is " << mute_state << ", AGC is " << agc_state;
  }
}

}  // namespace

void BasicTest::TearDown() {
  // Restore initial_gain_state_, if we changed the gain in this test case.
  if (stream_config().is_bound() && initial_gain_state_.has_value() && set_gain_state_) {
    LogGainState("Restoring previous gain: ", *initial_gain_state_);
    stream_config()->SetGain(std::move(*initial_gain_state_));
    initial_gain_state_.reset();
  }

  TestBase::TearDown();
}

// Requests on protocols that are composesd into StreamConfig/Dai/Codec/Composite.
//
// fuchsia.hardware.audio.Health
// We expect a response, and we allow 'healthy' to be either unspecified or TRUE.
void BasicTest::RequestHealthAndExpectHealthy() {
  GetHealthState(AddCallback("GetHealthState", [](fuchsia::hardware::audio::HealthState state) {
    EXPECT_TRUE(!state.has_healthy() || state.healthy());
  }));
  ExpectCallbacks();
}

void BasicTest::GetHealthState(fuchsia::hardware::audio::Health::GetHealthStateCallback cb) {
  if (device_entry().isCodec()) {
    codec()->GetHealthState(std::move(cb));
  } else if (device_entry().isComposite()) {
    composite()->GetHealthState(std::move(cb));
  } else if (device_entry().isDai()) {
    dai()->GetHealthState(std::move(cb));
  } else if (device_entry().isStreamConfig()) {
    stream_config()->GetHealthState(std::move(cb));
  }
}

// Basic (non-privileged) requests
//
// Request properties including unique ID (which should be unique across instances).
// The FIDL table for properties differs across Codec/Composite/Dai/StreamConfig.
// Extract these into a common struct so that subsequent code can be shared.
void BasicTest::RetrieveProperties() {
  properties().reset();
  // TODO(b/315049103): actually ensure that this differs between input and output.
  if (device_entry().isCodec()) {
    codec()->GetProperties(AddCallback(
        "Codec::GetProperties", [this](fuchsia::hardware::audio::CodecProperties props) {
          properties() = BaseProperties{};
          if (props.has_is_input()) {
            properties()->is_input = props.is_input();
          }
          if (props.has_unique_id()) {
            properties()->unique_id.emplace();
            std::memcpy(properties()->unique_id->data(), props.unique_id().data(), 16);
          }
          if (props.has_manufacturer()) {
            properties()->manufacturer = props.manufacturer();
          }
          if (props.has_product()) {
            properties()->product = props.product();
          }
          if (props.has_plug_detect_capabilities()) {
            properties()->plug_detect_capabilities = props.plug_detect_capabilities();
          }
        }));
  } else if (device_entry().isComposite()) {
    composite()->GetProperties(AddCallback(
        "Composite::GetProperties", [this](fuchsia::hardware::audio::CompositeProperties props) {
          properties() = BaseProperties{};
          if (props.has_unique_id()) {
            properties()->unique_id.emplace(props.unique_id());
          }
          if (props.has_manufacturer()) {
            properties()->manufacturer = props.manufacturer();
          }
          if (props.has_product()) {
            properties()->product = props.product();
          }
          if (props.has_clock_domain()) {
            properties()->clock_domain = props.clock_domain();
          }
        }));
  } else if (device_entry().isDai()) {
    dai()->GetProperties(
        AddCallback("Dai::GetProperties", [this](fuchsia::hardware::audio::DaiProperties props) {
          properties() = BaseProperties{};
          if (props.has_is_input()) {
            properties()->is_input = props.is_input();
          }
          if (props.has_unique_id()) {
            properties()->unique_id = props.unique_id();
          }
          if (props.has_manufacturer()) {
            properties()->manufacturer = props.manufacturer();
          }
          if (props.has_product_name()) {  // Note: not 'product'
            properties()->product = props.product_name();
          }
          if (props.has_clock_domain()) {
            properties()->clock_domain = props.clock_domain();
          }
        }));
  } else if (device_entry().isStreamConfig()) {
    stream_config()->GetProperties(AddCallback(
        "StreamConfig::GetProperties", [this](fuchsia::hardware::audio::StreamProperties props) {
          properties() = BaseProperties{};
          if (props.has_is_input()) {
            properties()->is_input = props.is_input();
          }
          if (props.has_unique_id()) {
            properties()->unique_id = props.unique_id();
          }
          if (props.has_manufacturer()) {
            properties()->manufacturer = props.manufacturer();
          }
          if (props.has_product()) {
            properties()->product = props.product();
          }
          if (props.has_clock_domain()) {
            properties()->clock_domain = props.clock_domain();
          }
          if (props.has_plug_detect_capabilities()) {
            properties()->plug_detect_capabilities = props.plug_detect_capabilities();
          }
          if (props.has_can_mute()) {
            properties()->can_mute = props.can_mute();
          }
          if (props.has_can_agc()) {
            properties()->can_agc = props.can_agc();
          }
          if (props.has_min_gain_db()) {
            properties()->min_gain_db = props.min_gain_db();
          }
          if (props.has_max_gain_db()) {
            properties()->max_gain_db = props.max_gain_db();
          }
          if (props.has_gain_step_db()) {
            properties()->gain_step_db = props.gain_step_db();
          }
        }));
  }
  ExpectCallbacks();
  EXPECT_TRUE(properties().has_value()) << "No GetProperties completion was received";
}

void BasicTest::ValidateProperties() {
  ASSERT_TRUE(properties().has_value());

  // The following fields are optional, but must be non-empty if they are specified.
  EXPECT_FALSE(properties()->manufacturer.has_value() && properties()->manufacturer->empty());
  EXPECT_FALSE(properties()->product.has_value() && properties()->product->empty());

  // Just check that required fields are present
  if (device_entry().isCodec()) {
    EXPECT_TRUE(properties()->plug_detect_capabilities.has_value());
  } else if (device_entry().isComposite()) {
    EXPECT_TRUE(properties()->clock_domain.has_value());
  } else if (device_entry().isDai()) {
    EXPECT_TRUE(properties()->is_input.has_value());
    EXPECT_TRUE(properties()->clock_domain.has_value());
  } else if (device_entry().isStreamConfig()) {
    ASSERT_TRUE(properties()->is_input.has_value());
    EXPECT_TRUE(properties()->clock_domain.has_value());
    EXPECT_TRUE(properties()->plug_detect_capabilities.has_value());
    ASSERT_TRUE(properties()->min_gain_db.has_value());
    ASSERT_TRUE(properties()->max_gain_db.has_value());
    ASSERT_TRUE(properties()->gain_step_db.has_value());

    // For StreamConfig, we can do additional data validity/range checks.
    EXPECT_EQ(*properties()->is_input, driver_type() == DriverType::StreamConfigInput);
    ASSERT_TRUE(std::isfinite(*properties()->min_gain_db)) << "irregular min_gain_db";
    ASSERT_TRUE(std::isfinite(*properties()->max_gain_db)) << "irregular max_gain_db";
    ASSERT_TRUE(std::isfinite(*properties()->gain_step_db)) << "irregular gain_step_db";
    EXPECT_LE(*properties()->min_gain_db, *properties()->max_gain_db) << "max_gain_db too small";
    EXPECT_GE(*properties()->gain_step_db, 0.0f) << "gain_step_db too small";
    EXPECT_LE(*properties()->gain_step_db, *properties()->max_gain_db - *properties()->min_gain_db)
        << "gain_step_db too large";
  } else {
    FAIL() << "Unknown device type";
  }
}

// Request that the driver return its gain capabilities and current state, expecting a response.
// TODO(b/315051281): If possible, combine this with the corresponding check of the signalprocessing
// gain element, once that test exists.
void BasicTest::WatchGainStateAndExpectUpdate() {
  ASSERT_TRUE(properties().has_value());
  ASSERT_TRUE(device_entry().isStreamConfig());

  // We reconnect the stream every time we run a test, and by driver interface definition the driver
  // must reply to the first watch request, so we get gain state by issuing a watch FIDL call.
  stream_config()->WatchGainState(
      AddCallback("WatchGainState", [this](fuchsia::hardware::audio::GainState gain_state) {
        LogGainState((initial_gain_state_.has_value() ? "Received gain update:  "
                                                      : "Storing previous gain: "),
                     gain_state);

        ASSERT_TRUE(gain_state.has_gain_db());
        EXPECT_GE(gain_state.gain_db(), *properties()->min_gain_db);
        EXPECT_LE(gain_state.gain_db(), *properties()->max_gain_db);

        // If we're muted, then we must be capable of muting.
        EXPECT_TRUE(!gain_state.has_muted() || !gain_state.muted() || *properties()->can_mute);
        // If AGC is enabled, then we must be capable of AGC.
        EXPECT_TRUE(!gain_state.has_agc_enabled() || !gain_state.agc_enabled() ||
                    *properties()->can_agc);
        if (!initial_gain_state_.has_value()) {
          initial_gain_state_ = std::move(gain_state);
        }
      }));
  ExpectCallbacks();
}

// Request that the driver return its current gain state, expecting no response (no change).
// TODO(b/315051281): If possible, combine this with the corresponding check of the signalprocessing
// gain element, once that test exists.
void BasicTest::WatchGainStateAndExpectNoUpdate() {
  ASSERT_TRUE(device_entry().isStreamConfig());

  stream_config()->WatchGainState([](fuchsia::hardware::audio::GainState gain_state) {
    FAIL() << "Unexpected gain update received";
  });
}

// Determine an appropriate gain state to request, then call other method to request that driver set
// gain. This method assumes that the driver already successfully responded to a GetInitialGainState
// request. If this device's gain is fixed and cannot be changed, then SKIP the test.
// TODO(b/315051281): If possible, combine this with the corresponding check of the signalprocessing
// gain element, once that test exists.
void BasicTest::RequestSetGain() {
  ASSERT_TRUE(device_entry().isStreamConfig()) << __func__ << ": device_entry is not StreamConfig";
  ASSERT_TRUE(properties().has_value());
  if (*properties()->max_gain_db == *properties()->min_gain_db && !*properties()->can_mute &&
      !*properties()->can_agc) {
    GTEST_SKIP() << "*** Audio " << driver_type() << " has fixed gain ("
                 << initial_gain_state_->gain_db()
                 << " dB) and cannot MUTE or AGC. Skipping SetGain test. ***";
  }

  // Ensure we've retrieved initial gain settings, so we can restore them after this test case.
  ASSERT_TRUE(initial_gain_state_.has_value());

  // Base our new gain settings on the old ones, to avoid existing values.
  fuchsia::hardware::audio::GainState gain_state_to_set;
  ASSERT_EQ(initial_gain_state_->Clone(&gain_state_to_set), ZX_OK);
  // Base our new gain settings on the old ones: avoid existing values so this Set is a change.
  // If we got this far, we know we can change something (even if it isn't gain_db).
  // Change to a different gain_db.
  *gain_state_to_set.mutable_gain_db() =
      (initial_gain_state_->gain_db() == *properties()->min_gain_db ? *properties()->max_gain_db
                                                                    : *properties()->min_gain_db);
  // Toggle muted if we can change it (explicitly set it to false, if we can't).
  *gain_state_to_set.mutable_muted() =
      *properties()->can_mute && !(gain_state_to_set.has_muted() && gain_state_to_set.muted());
  // Toggle AGC if we can change it (explicitly set it to false, if we can't).
  *gain_state_to_set.mutable_agc_enabled() =
      *properties()->can_agc &&
      !(gain_state_to_set.has_agc_enabled() && gain_state_to_set.agc_enabled());

  set_gain_state_ = true;
  LogGainState("SetGain about to set:  ", gain_state_to_set);
  stream_config()->SetGain(std::move(gain_state_to_set));
}

// TODO(b/315051014): If possible, combine this with the corresponding plug check of the
// signalprocessing endpoint element, once that test exists.
void BasicTest::ValidatePlugState(const fuchsia::hardware::audio::PlugState& plug_state) {
  ASSERT_TRUE(plug_state.has_plugged());
  if (!plug_state.plugged()) {
    ASSERT_TRUE(properties().has_value());
    ASSERT_TRUE(properties()->plug_detect_capabilities.has_value());
    EXPECT_NE(*properties()->plug_detect_capabilities,
              fuchsia::hardware::audio::PlugDetectCapabilities::HARDWIRED)
        << "Device reported plug capabilities as HARDWIRED, but now reports as unplugged";
  }

  EXPECT_TRUE(plug_state.has_plug_state_time());
  EXPECT_GE(plug_state.plug_state_time(), 0u);
  EXPECT_LT(plug_state.plug_state_time(), zx::clock::get_monotonic().get());
}

// Request that the driver return its current plug state, expecting a valid response.
// TODO(b/315051014): If possible, combine this with the corresponding plug check of the
// signalprocessing endpoint element, once that test exists.
void BasicTest::WatchPlugStateAndExpectUpdate() {
  ASSERT_TRUE(properties().has_value());

  // Since we reconnect to the audio stream every time we run this test and we are guaranteed by
  // the audio driver interface definition that the driver will reply to the first watch request,
  // we can get the plug state by issuing a watch FIDL call.
  fuchsia::hardware::audio::PlugState initial_plug_state;
  if (device_entry().isCodec()) {
    codec()->WatchPlugState(AddCallback(
        "Codec::WatchPlugState", [&initial_plug_state](fuchsia::hardware::audio::PlugState state) {
          initial_plug_state = std::move(state);
        }));
  } else if (device_entry().isStreamConfig()) {
    stream_config()->WatchPlugState(
        AddCallback("StreamConfig::WatchPlugState",
                    [&initial_plug_state](fuchsia::hardware::audio::PlugState state) {
                      initial_plug_state = std::move(state);
                    }));
  } else {
    FAIL() << "Wrong device type for " << __func__;
  }
  ExpectCallbacks();
  if (!HasFailure()) {
    ValidatePlugState(initial_plug_state);
  }
}

// Request that the driver return its current plug state, expecting no response (no change).
// TODO(b/315051014): If possible, combine this with the corresponding plug check of the
// signalprocessing endpoint element, once that test exists.
void BasicTest::WatchPlugStateAndExpectNoUpdate() {
  if (device_entry().isCodec()) {
    codec()->WatchPlugState([](fuchsia::hardware::audio::PlugState state) {
      FAIL() << "Codec::WatchPlugState: unexpected plug update received";
    });
  } else if (device_entry().isStreamConfig()) {
    stream_config()->WatchPlugState([](fuchsia::hardware::audio::PlugState state) {
      FAIL() << "StreamConfig::WatchPlugState: unexpected plug update received";
    });
  } else {
    FAIL() << "Wrong device type for " << __func__;
  }
}

#define DEFINE_BASIC_TEST_CLASS(CLASS_NAME, CODE)                               \
  class CLASS_NAME : public BasicTest {                                         \
   public:                                                                      \
    explicit CLASS_NAME(const DeviceEntry& dev_entry) : BasicTest(dev_entry) {} \
    void TestBody() override { CODE }                                           \
  }

// Test cases that target each of the various Stream channel commands

// Verify the driver responds to the GetHealthState query.
DEFINE_BASIC_TEST_CLASS(Health, { RequestHealthAndExpectHealthy(); });

// Verify a valid unique_id, manufacturer, product and gain capabilities is successfully received.
DEFINE_BASIC_TEST_CLASS(GetProperties, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ValidateProperties();
});

// Verify the initial WatchGainState responses are successfully received.
DEFINE_BASIC_TEST_CLASS(GetInitialGainState, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());

  WatchGainStateAndExpectUpdate();
  WaitForError();
});

// Verify that no response is received, for a subsequent WatchGainState request.
DEFINE_BASIC_TEST_CLASS(WatchGainSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchGainStateAndExpectUpdate());

  WatchGainStateAndExpectNoUpdate();
  WaitForError();
});

// Verify valid set gain responses are successfully received.
DEFINE_BASIC_TEST_CLASS(SetGain, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchGainStateAndExpectUpdate());

  RequestSetGain();
  WaitForError();
});

// Verify that format-retrieval responses are successfully received and are complete and valid.
DEFINE_BASIC_TEST_CLASS(RingBufferFormats, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveRingBufferFormats());
  WaitForError();
});

// Verify that format-retrieval responses are successfully received and are complete and valid.
DEFINE_BASIC_TEST_CLASS(DaiFormats, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveDaiFormats());
  WaitForError();
});

// Verify that a valid initial plug detect response is successfully received.
DEFINE_BASIC_TEST_CLASS(GetInitialPlugState, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());

  WatchPlugStateAndExpectUpdate();
  WaitForError();

  // Someday: determine how to trigger the driver's internal hardware-detect mechanism, so it
  // emits unsolicited PLUG/UNPLUG events -- otherwise driver plug detect updates are not fully
  // testable.
});

// Verify that no response is received, for a subsequent WatchPlugState request.
DEFINE_BASIC_TEST_CLASS(WatchPlugSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RetrieveProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchPlugStateAndExpectUpdate());

  WatchPlugStateAndExpectNoUpdate();
  WaitForError();
});

// Verify that a valid toplogy list is successfully received.
DEFINE_BASIC_TEST_CLASS(GetTopologies, { RequestTopologies(); });

// Verify that a valid toplogy is successfully received.
DEFINE_BASIC_TEST_CLASS(GetTopology, {
  RequestTopologies();
  RequestTopology();
});

// Register separate test case instances for each enumerated device
//
// See googletest/docs/advanced.md for details
#define REGISTER_BASIC_TEST(CLASS_NAME, DEVICE)                                                \
  {                                                                                            \
    testing::RegisterTest("BasicTest", TestNameForEntry(#CLASS_NAME, DEVICE).c_str(), nullptr, \
                          DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                 \
                          [&]() -> BasicTest* { return new CLASS_NAME(DEVICE); });             \
  }

void RegisterBasicTestsForDevice(const DeviceEntry& device_entry) {
  if (device_entry.isCodec()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(GetProperties, device_entry);
    REGISTER_BASIC_TEST(DaiFormats, device_entry);
    REGISTER_BASIC_TEST(GetInitialPlugState, device_entry);
    REGISTER_BASIC_TEST(WatchPlugSecondTimeNoResponse, device_entry);
  } else if (device_entry.isComposite()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(GetTopologies, device_entry);
    REGISTER_BASIC_TEST(GetTopology, device_entry);
    REGISTER_BASIC_TEST(GetProperties, device_entry);
    REGISTER_BASIC_TEST(RingBufferFormats, device_entry);
    REGISTER_BASIC_TEST(DaiFormats, device_entry);
  } else if (device_entry.isDai()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(GetProperties, device_entry);
    REGISTER_BASIC_TEST(RingBufferFormats, device_entry);
    REGISTER_BASIC_TEST(DaiFormats, device_entry);
  } else if (device_entry.isStreamConfig()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(GetProperties, device_entry);
    REGISTER_BASIC_TEST(GetInitialGainState, device_entry);
    REGISTER_BASIC_TEST(WatchGainSecondTimeNoResponse, device_entry);
    REGISTER_BASIC_TEST(SetGain, device_entry);
    REGISTER_BASIC_TEST(RingBufferFormats, device_entry);
    REGISTER_BASIC_TEST(GetInitialPlugState, device_entry);
    REGISTER_BASIC_TEST(WatchPlugSecondTimeNoResponse, device_entry);
  } else {
    FAIL() << "Unknown device type for entry '" << device_entry.filename << "'";
  }
}

// TODO(https://fxbug.dev/42075676): Add testing for Composite protocol methods.

// TODO(b/302704556): Add tests for Watch-while-still-pending (specifically WatchGainState,
//   WatchPlugState, WatchClockRecoveryPositionInfo, WatchDelayInfo, WatchElementState and
//   WatchTopology).

// TODO(https://fxbug.dev/42077405): Add testing for SignalProcessing methods.
//
// Proposed test cases for fuchsia.hardware.audio.signalprocessing listed below:
// BasicTest cases:
// SignalProcessingSupport
//    SignalProcessingConnector::SignalProcessingConnect returns and does not close channel.
//    child protocol channel stays bound if supported, and closes with ZX_ERR_NOT_SUPPORTED if
//    not.
// SignalProcessingElements
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetElements closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetElements returns a vector with [1,64] entries.
//    Implies that GetTopologies must return a non-empty vector.
//    For each element:
//      id and type are required.
//      ElementType matches the TypeSpecificElement.
//      Save the elements in a set, for recognition in later cases.
// SignalProcessingTopologies
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetTopologies closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetTopologies returns a vector with [1,64] entries
//    Implies that GetElements must return a non-empty vector.
//    WatchTopology returns a value that is in the range returned by GetTopologies.
//    For each topology element:
//        id and processing_elements_edge_pairs are required.
//    For each processing_elements_edge_pairs entry:
//        processing_element_id_from and processing_element_id_to are both known (in elements
//        set).
// WatchTopologyWhilePending
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetTopologies closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetTopologies returns a vector with [1,64] entries
//    WatchTopology returns a value that is in the range returned by GetTopologies.
//    WatchTopology (again) closes the protocol channel with ZX_ERR_BAD_STATE
// InitialElementState
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other
//    error. Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    Callback contains a valid complete ElementState that matches the ElementType.
// WatchElementStateBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    WatchElementState(badId) returns ZX_ERR_INVALID_ARGS and does not close. Fail on other
//    error.
// WatchElementStateWhilePending
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other
//    error. Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    WatchElementState (again) closes the protocol channel with ZX_ERR_BAD_STATE

// AdminTest cases:
// SetTopologySupported
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetTopology returns callback.
//    WatchTopology acknowledges the change made by SetTopology.
// SetTopologyBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetTopology(badId) returns ZX_ERR_INVALID_ARGS, does not close channel. Fail on other error.
//    WatchTopology does not return.
// SetTopologyInvalidated
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    First make a change that invalidates the SignalProcessing configuration, then
//    WatchTopology should ... return ZX_ERR_BAD_STATE and not close channel?
//    SetTopology should return ZX_ERR_BAD_STATE and not close channel.
// SetTopologyReconfigured
//    If SignalProcessingConnect not supported earlier, SKIP.
//    First invalidate the SignalProcessing configuration, then retrieve the new topologies.
//    SetTopology returns callback (does not fail or close channel).
//    WatchTopology acknowledges the change made by SetTopology.
// SetElementState
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState returns callback.  Any other observable state?
// SetElementStateElementSpecific
//    Detailed checks of specific input or output fields that are unique to the element type.
//    ... likely multiple test cases here, one for each ElementType
// SetElementStateNoChange
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState does not returns callback, trigger WatchElementStateChange or close channel.
// SetElementStateBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badId) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateBadValues
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badVal) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateInvalidated
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    First make a change that invalidates the SignalProcessing configuration, then
//    SetElementState should return ZX_ERR_BAD_STATE and not close channel.
// SetElementStateReconfigured
//    If SignalProcessingConnect not supported earlier, SKIP.
//    First invalidate the SignalProcessing configuration, then retrieve new elements/topologies.
//    SetElementState returns callback (does not fail or close channel).
// WatchElementStateChange
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState pends until SetElementState is called.
//    Upon change, returns callback with values that match SetElementState.

}  // namespace media::audio::drivers::test
