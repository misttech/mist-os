// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_TEST_TEST_BASE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_TEST_TEST_BASE_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/device/audio.h>

#include "src/media/audio/drivers/test/audio_device_enumerator_stub.h"
#include "src/media/audio/lib/test/test_fixture.h"

namespace media::audio::drivers::test {

// We enable top-level methods (e.g. TestBase::RequestFormats, BasicTest::RequestStreamProperties,
// AdminTest::RequestBuffer) to skip or produce multiple errors and then cause a test case to
// exit-early once they return, even if no fatal errors were triggered.
// Gtest defines NO macro for this case -- only ASSERT_NO_FATAL_FAILURE -- so we define our own.
// Macro definition in headers is discouraged (at best), but this is used in local test code only.
#define ASSERT_NO_FAILURE_OR_SKIP(statement, ...)          \
  do {                                                     \
    statement;                                             \
    if (TestBase::HasFailure() || TestBase::IsSkipped()) { \
      return;                                              \
    }                                                      \
  } while (0)

enum DriverType : uint16_t { StreamConfigInput = 0, StreamConfigOutput = 1, Dai = 2 };
inline std::ostream& operator<<(std::ostream& out, const DriverType& dev_dir) {
  switch (dev_dir) {
    case DriverType::StreamConfigInput:
      return (out << "StreamConfig(In)");
    case DriverType::StreamConfigOutput:
      return (out << "StreamConfig(Out)");
    case DriverType::Dai:
      return (out << "Dai");
  }
}

enum DeviceType : uint16_t { BuiltIn = 0, Virtual = 1, A2DP = 2 };
inline std::ostream& operator<<(std::ostream& out, const DeviceType& device_type) {
  switch (device_type) {
    case DeviceType::BuiltIn:
      return (out << "Built-in");
    case DeviceType::Virtual:
      return (out << "VirtualAudio");
    case DeviceType::A2DP:
      return (out << "A2DP");
  }
}

struct DeviceEntry {
  std::variant<std::monostate, fidl::UnownedClientEnd<fuchsia_io::Directory>> dir;
  std::string filename;
  DriverType driver_type;
  DeviceType device_type;

  bool isA2DP() const { return device_type == DeviceType::A2DP; }
  bool isStreamConfig() const {
    return driver_type == DriverType::StreamConfigInput ||
           driver_type == DriverType::StreamConfigOutput;
  }
  bool isDai() const { return driver_type == DriverType::Dai; }

  bool operator<(const DeviceEntry& rhs) const {
    return std::tie(dir, filename, driver_type, device_type) <
           std::tie(rhs.dir, rhs.filename, rhs.driver_type, rhs.device_type);
  }
};

// Used in registering separate test case instances for each enumerated device
//
// See googletest/docs/advanced.md for details
//
// Devices are displayed in the 'audio-output/000' format, or simply the filename, if the
// special dir_fd value is observed (an example might be 'Bluetooth-A2DP' for Bluetooth devices).
std::string inline DevNameForEntry(const DeviceEntry& device_entry) {
  std::string device_name =
      (device_entry.device_type == DeviceType::Virtual ? "Virtual" : device_entry.filename);

  switch (device_entry.driver_type) {
    case DriverType::StreamConfigInput:
      return "audio-input/" + device_name;
    case DriverType::StreamConfigOutput:
      return "audio-output/" + device_name;
    case DriverType::Dai:
      return "dai/" + device_name;
  }
}
std::string inline TestNameForEntry(const std::string& test_class_name,
                                    const DeviceEntry& device_entry) {
  return DevNameForEntry(device_entry) + ":" + test_class_name;
}

class TestBase : public media::audio::test::TestFixture {
 public:
  explicit TestBase(const DeviceEntry& device_entry) : device_entry_(device_entry) {}

 protected:
  void SetUp() override;
  void TearDown() override;

  void ConnectToStreamConfigDevice(const DeviceEntry& device_entry);
  void ConnectToDaiDevice(const DeviceEntry& device_entry);
  void ConnectToBluetoothDevice();
  void CreateStreamConfigFromChannel(
      fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> channel);
  void CreateDaiFromChannel(fidl::InterfaceHandle<fuchsia::hardware::audio::Dai> channel);

  const DeviceEntry& device_entry() const { return device_entry_; }
  DriverType driver_type() const { return device_entry_.driver_type; }
  DeviceType device_type() const { return device_entry_.device_type; }

  // "Basic" (stream-config channel) tests and "Admin" (ring-buffer channel) tests both need to know
  // the supported formats, so this is implemented in the shared base class.
  void RequestFormats();

  // TODO(fxbug.dev/83972): Consider a more functional style when validating formats
  const std::vector<fuchsia::hardware::audio::PcmSupportedFormats>& ring_buffer_pcm_formats()
      const {
    return ring_buffer_pcm_formats_;
  }

  fidl::InterfacePtr<fuchsia::hardware::audio::StreamConfig>& stream_config() {
    return stream_config_;
  }
  fidl::InterfacePtr<fuchsia::hardware::audio::Dai>& dai() { return dai_; }

  const fuchsia::hardware::audio::PcmFormat& min_ring_buffer_format() const {
    return min_ring_buffer_format_;
  }
  const fuchsia::hardware::audio::PcmFormat& max_ring_buffer_format() const {
    return max_ring_buffer_format_;
  }
  void SetMinRingBufferFormat(fuchsia::hardware::audio::PcmFormat& pcm_format) const {
    pcm_format = min_ring_buffer_format();
  }
  void SetMaxRingBufferFormat(fuchsia::hardware::audio::PcmFormat& pcm_format) const {
    pcm_format = max_ring_buffer_format();
  }
  void SetMinDaiFormat(fuchsia::hardware::audio::DaiFormat& dai_format) const {
    EXPECT_EQ(fuchsia::hardware::audio::Clone(min_dai_format_, &dai_format), ZX_OK);
  }
  void SetMaxDaiFormat(fuchsia::hardware::audio::DaiFormat& dai_format) const {
    EXPECT_EQ(fuchsia::hardware::audio::Clone(max_dai_format_, &dai_format), ZX_OK);
  }
  static void LogFormat(const fuchsia::hardware::audio::PcmFormat& format,
                        const std::string& tag = {});

  void WaitForError(zx::duration wait_duration = kWaitForErrorDuration) {
    RunLoopWithTimeoutOrUntil([]() { return HasFailure() || IsSkipped(); }, wait_duration);
  }

 private:
  static constexpr zx::duration kWaitForErrorDuration = zx::msec(100);

  void SetMinMaxFormats();
  void SetMinMaxRingBufferFormats();
  void SetMinMaxDaiFormats();

  std::optional<component_testing::RealmRoot> realm_;
  fuchsia::component::BinderPtr audio_binder_;

  const DeviceEntry& device_entry_;

  fidl::InterfacePtr<fuchsia::hardware::audio::StreamConfig> stream_config_;
  fidl::InterfacePtr<fuchsia::hardware::audio::Dai> dai_;

  std::vector<fuchsia::hardware::audio::PcmSupportedFormats> ring_buffer_pcm_formats_;
  std::vector<fuchsia::hardware::audio::DaiSupportedFormats> dai_formats_;

  fuchsia::hardware::audio::PcmFormat min_ring_buffer_format_;
  fuchsia::hardware::audio::PcmFormat max_ring_buffer_format_;
  fuchsia::hardware::audio::DaiFormat min_dai_format_;
  fuchsia::hardware::audio::DaiFormat max_dai_format_;
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TEST_TEST_BASE_H_
