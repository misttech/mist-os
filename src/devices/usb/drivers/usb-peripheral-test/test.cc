// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <getopt.h>
#include <lib/fit/defer.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <numeric>
#include <span>
#include <thread>

#include <fbl/algorithm.h>
#include <usb/peripheral-test.h>
#include <usb/peripheral.h>
#include <usbhost/usbhost.h>
#include <zxtest/zxtest.h>

namespace {

struct usb_device* dev = nullptr;
struct usb_endpoint_descriptor* bulk_out_ep = nullptr;
struct usb_endpoint_descriptor* bulk_in_ep = nullptr;
struct usb_endpoint_descriptor* intr_in_ep = nullptr;

static uint8_t test_interface;
constexpr uint64_t kUsbTimoutMilliseconds = 1000;  // 1 second
constexpr uint64_t kSecondsToNanoseconds = 1'000'000'000;
constexpr uint64_t kMicrosecondsToNanoseconds = 1000;
constexpr uint64_t kNanosecondsToMilliseconds = 1'000'000;
constexpr uint64_t kBulkRequestSize = 512;

constexpr char kUsageMsg[] = R"(
    [OPTIONS]
    --help -help [-h]                             Prints this message.
    --time -time [-t]                             Configurable time for tests to run.
    --bytes -bytes [-b]                           Conifgurable transfer bytes for data transfer.
    --bulk-iterations -bulk-iterations [-i]       Configurable iteration for bulk transfer.
    --buffersize -buffersize [-s]                 Configurable buffer size. Default buffer size set to 4096 bytes.
    --retest -retest [-r]                         Configurable retest transfer type. Specify one of 
                                                  {CONTROL, INTERRUPT, BULK} to retest transfer type. 
    --retest-iterations -retest-iterations [-n]   Configurable number of times the transfer type is tested.                                  
)";

class UsbPeripheralConfigurableTests : public ::zxtest::Test {
 public:
  // User configurable time (in milliseconds) for USB peripheral tests.
  inline static int64_t time_ms_ = 5000;

  // User configurable transfer byte size.
  inline static size_t bytes_ = 64;

  // User configurable iterations for bulk transfer test.
  inline static int bulk_iterations_ = 5;

  // User configurable buffer size.
  inline static uint64_t buffer_size_ = 4096;

  // User configurable to repeat test.
  enum class TransferOptions { CONTROL, INTERRUPT, BULK, NONE };
  inline static TransferOptions retest_config_ = TransferOptions::BULK;
  inline static int retest_iterations_ = 2;
  static zx_status_t SetRetestConfigFromString(const std::string& str);

  // Help option.
  inline static bool help_ = false;
};

zx_status_t UsbPeripheralConfigurableTests::SetRetestConfigFromString(const std::string& str) {
  if (str == "CONTROL") {
    retest_config_ = UsbPeripheralConfigurableTests::TransferOptions::CONTROL;
    return ZX_OK;
  } else if (str == "INTERRUPT") {
    retest_config_ = UsbPeripheralConfigurableTests::TransferOptions::INTERRUPT;
    return ZX_OK;
  } else if (str == "BULK") {
    retest_config_ = UsbPeripheralConfigurableTests::TransferOptions::BULK;
    return ZX_OK;
  } else {
    retest_config_ = UsbPeripheralConfigurableTests::TransferOptions::NONE;
    return ZX_ERR_INVALID_ARGS;
  }
}

class Timer {
 public:
  void Start() { start_ = GetCurrentTimeNanoseconds(); }
  int64_t Stop() {
    stop_ = GetTime();
    return stop_;
  }
  // Gets the amount of milliseconds since |start_|.
  int64_t GetElapsedTimeMilliseconds() const {
    return (GetCurrentTimeNanoseconds() - start_) / kNanosecondsToMilliseconds;
  }

 private:
  int64_t start_;
  int64_t stop_;
  static int64_t GetCurrentTimeNanoseconds() {
    struct timeval tv;
    if (gettimeofday(&tv, nullptr) < 0) {
      return 0;
    }
    return tv.tv_sec * kSecondsToNanoseconds + tv.tv_usec * kMicrosecondsToNanoseconds;
  }
  int64_t GetTime() const {
    return (GetCurrentTimeNanoseconds() - start_) / kNanosecondsToMilliseconds;
  }
};

void pattern_buffer(cpp20::span<uint8_t> container, uint8_t distinct_buffer_number) {
  // Generate a known pattern for the buffer.
  uint64_t container_size = container.size();
  for (size_t i = 0; i < container_size - 1; i += 2) {
    container[i] = 0x1;
    container[i + 1] = distinct_buffer_number - 1;
  }
  // Setting buffer borders
  container.front() = distinct_buffer_number;
  container.back() = distinct_buffer_number;
}

// Tests control and interrupt transfers with specified transfer size.
void control_interrupt_test(size_t transfer_size) {
  uint8_t distinct_buffer_number = 3;
  std::unique_ptr<uint8_t[]> send_buffer = std::make_unique<uint8_t[]>(transfer_size);
  std::unique_ptr<uint8_t[]> receive_buffer = std::make_unique<uint8_t[]>(transfer_size);

  pattern_buffer({send_buffer.get(), transfer_size}, distinct_buffer_number);

  Timer timer;
  timer.Start();

  // Send data to device via OUT control request.
  int ret = usb_device_control_transfer(
      dev, USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE, USB_PERIPHERAL_TEST_SET_DATA, 0,
      test_interface, send_buffer.get(), static_cast<int>(transfer_size), kUsbTimoutMilliseconds);
  ASSERT_EQ(ret, static_cast<int>(transfer_size));

  // Receive data back from device via IN control request.
  ret = usb_device_control_transfer(dev, USB_DIR_IN | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
                                    USB_PERIPHERAL_TEST_GET_DATA, 0, test_interface,
                                    receive_buffer.get(), static_cast<int>(transfer_size),
                                    kUsbTimoutMilliseconds);
  ASSERT_EQ(ret, static_cast<int>(transfer_size));

  // Sent and received data should match.
  EXPECT_EQ(memcmp(send_buffer.get(), receive_buffer.get(), transfer_size), 0);

  // Create a thread to wait for interrupt request.
  auto thread_func = [](struct usb_request** req) -> void {
    *req = usb_request_wait(dev, kUsbTimoutMilliseconds);
  };

  struct usb_request* complete_req = nullptr;
  std::thread wait_thread(thread_func, &complete_req);

  // Queue read for interrupt request
  auto* req = usb_request_new(dev, intr_in_ep);
  EXPECT_NE(req, nullptr);
  req->buffer = receive_buffer.get();
  req->buffer_length = static_cast<int>(transfer_size);
  ret = usb_request_queue(req);
  EXPECT_EQ(ret, 0);

  // Ask the device to send us an interrupt request containing the data we sent earlier.
  ret = usb_device_control_transfer(dev, USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
                                    USB_PERIPHERAL_TEST_SEND_INTERUPT, 0, test_interface, nullptr,
                                    0, kUsbTimoutMilliseconds);
  ASSERT_EQ(ret, 0);

  wait_thread.join();

  EXPECT_EQ(complete_req, req);
  EXPECT_EQ(static_cast<size_t>(req->actual_length), transfer_size);

  // Sent data should match payload of interrupt request.
  EXPECT_EQ(memcmp(send_buffer.get(), receive_buffer.get(), transfer_size), 0);

  usb_request_free(req);

  const int64_t elapsed_time = timer.Stop();
  printf("[          ] Transferred %zu bytes/%lums\n", transfer_size, elapsed_time);
}

// Tests bulk transfers with specified transfer size.
void bulk_test(uint64_t buffer_size, size_t bulk_iterations) {
  buffer_size = fbl::round_up(buffer_size, kBulkRequestSize);
  uint8_t distinct_buffer_number = 3;
  std::unique_ptr<uint8_t[]> send_buffer = std::make_unique<uint8_t[]>(buffer_size);
  std::unique_ptr<uint8_t[]> receive_buffer = std::make_unique<uint8_t[]>(buffer_size);

  // Initialize the buffer
  memset(send_buffer.get(), 9, buffer_size);
  memset(receive_buffer.get(), 9, buffer_size);

  auto* send_req = usb_request_new(dev, bulk_out_ep);
  EXPECT_NE(send_req, nullptr);
  send_req->buffer = send_buffer.get();
  send_req->buffer_length = static_cast<int>(buffer_size);

  auto* receive_req = usb_request_new(dev, bulk_in_ep);
  EXPECT_NE(receive_req, nullptr);
  receive_req->buffer = receive_buffer.get();
  receive_req->buffer_length = static_cast<int>(buffer_size);

  for (size_t i = 0; i < bulk_iterations; i++) {
    pattern_buffer({send_buffer.get(), buffer_size}, distinct_buffer_number);

    // Create a thread to wait for request completions.
    auto thread_func = [](struct usb_request** reqs) -> void {
      *reqs++ = usb_request_wait(dev, kUsbTimoutMilliseconds);
      *reqs = usb_request_wait(dev, kUsbTimoutMilliseconds);
    };

    struct usb_request* complete_reqs[2] = {};
    std::thread wait_thread(thread_func, complete_reqs);

    // Queue requests in both directions
    int ret = usb_request_queue(send_req);
    EXPECT_EQ(ret, 0);
    ret = usb_request_queue(receive_req);
    EXPECT_EQ(ret, 0);

    wait_thread.join();

    EXPECT_NE(complete_reqs[0], nullptr);
    EXPECT_NE(complete_reqs[1], nullptr);

    // Sent and received data should match.
    EXPECT_EQ(memcmp(send_buffer.get(), receive_buffer.get(), buffer_size), 0);

    // Changing the number for the next buffer and bulk iteration. Helps verify that each transfer
    // is different.
    if (distinct_buffer_number > 9)
      distinct_buffer_number = 3;
    else
      distinct_buffer_number++;
  }

  usb_request_free(send_req);
  usb_request_free(receive_req);
}

// ====================== User configurable tests ===================================

// Tests bulk transfer for long periods of time. Time is user determined.
TEST_F(UsbPeripheralConfigurableTests, stress_test_configurable) {
  int times_ran = 0;
  std::vector<int64_t> recorded_times;
  Timer timer_loop;
  Timer timer_recording;
  timer_loop.Start();

  while (timer_loop.GetElapsedTimeMilliseconds() < time_ms_) {
    timer_recording.Start();
    ASSERT_NO_FATAL_FAILURE(bulk_test(buffer_size_, bulk_iterations_));
    recorded_times.push_back(timer_recording.Stop());
    times_ran++;
  }

  int64_t elapsed_time = timer_loop.Stop();
  uint64_t total_transfer_bytes = times_ran * buffer_size_ * bulk_iterations_;
  printf("[          ] Bulk test ran %d times in %lums\n", times_ran, elapsed_time);
  printf("[          ] Total bytes transferred: %lu\n", total_transfer_bytes);

  if (!recorded_times.empty()) {
    uint64_t average =
        std::reduce(recorded_times.begin(), recorded_times.end()) / recorded_times.size();
    printf("[          ] - Average transfer: %lu/%ldms per bulk test call\n",
           buffer_size_ * bulk_iterations_, average);
  }
  EXPECT_FALSE(recorded_times.empty());
}

// Tests the repetition of a transfer type test.
TEST_F(UsbPeripheralConfigurableTests, retest_transfer_type_test) {
  std::vector<int64_t> recorded_times;
  Timer timer;
  switch (retest_config_) {
    case TransferOptions::CONTROL:
    case TransferOptions::INTERRUPT:
      for (int i = 0; i < retest_iterations_; i++) {
        timer.Start();
        ASSERT_NO_FATAL_FAILURE(control_interrupt_test(bytes_));
        recorded_times.push_back(timer.Stop());
      }
      break;
    case TransferOptions::BULK:
      for (int i = 0; i < retest_iterations_; i++) {
        timer.Start();
        ASSERT_NO_FATAL_FAILURE(bulk_test(buffer_size_, bulk_iterations_));
        recorded_times.push_back(timer.Stop());
      }
      break;
    default:
      fprintf(stderr, "[          ] Transfer type does not exist.\n");
      break;
  }

  if (!recorded_times.empty()) {
    uint64_t average =
        std::reduce(recorded_times.begin(), recorded_times.end()) / recorded_times.size();
    printf("[          ] - Average time for %d iterations is: %ldms\n", retest_iterations_,
           average);
  }
  EXPECT_FALSE(recorded_times.empty());
}

// Tests bulk OUT and IN transfers from user configurable data.
TEST_F(UsbPeripheralConfigurableTests, bulk_test) {
  ASSERT_NO_FATAL_FAILURE(bulk_test(buffer_size_, bulk_iterations_));
}

// Test control and interrupt requests from the user configurable data
TEST_F(UsbPeripheralConfigurableTests, control_interrupt_test_configurable) {
  ASSERT_NO_FATAL_FAILURE(control_interrupt_test(bytes_));
}

// =============================== Fixed tests ===================================
// Test control and interrupt requests with 8 byte transfer size.
TEST(UsbPeripheralFixedTests, control_interrupt_test_8) {
  ASSERT_NO_FATAL_FAILURE(control_interrupt_test(8));
}

// Test control and interrupt requests with 256 byte transfer size.
TEST(UsbPeripheralFixedTests, control_interrupt_test_256) {
  ASSERT_NO_FATAL_FAILURE(control_interrupt_test(256));
}

// Test control and interrupt requests with 1024 byte transfer size.
TEST(UsbPeripheralFixedTests, control_interrupt_test_1024) {
  ASSERT_NO_FATAL_FAILURE(control_interrupt_test(1024));
}

// usb_host_load() will call this for all connected USB devices.
int usb_device_added(const char* dev_name, void* client_data) {
  usb_descriptor_iter iter;
  struct usb_descriptor_header* header;
  struct usb_interface_descriptor* intf = nullptr;
  int ret;

  dev = usb_device_open(dev_name);
  if (!dev) {
    fprintf(stderr, "usb_device_open failed for %s\n", dev_name);
    return 0;
  }

  auto cleanup = fit::defer([]() {
    usb_device_close(dev);
    dev = nullptr;
  });

  uint16_t vid = usb_device_get_vendor_id(dev);
  uint16_t pid = usb_device_get_product_id(dev);

  if (vid != GOOGLE_USB_VID ||
      (pid != GOOGLE_USB_FUNCTION_TEST_PID && pid != GOOGLE_USB_CDC_AND_FUNCTION_TEST_PID)) {
    // Device doesn't match, so keep looking.
    return 0;
  }

  usb_descriptor_iter_init(dev, &iter);

  while ((header = usb_descriptor_iter_next(&iter)) != nullptr) {
    if (header->bDescriptorType == USB_DT_INTERFACE) {
      intf = reinterpret_cast<struct usb_interface_descriptor*>(header);
    } else if (header->bDescriptorType == USB_DT_ENDPOINT) {
      auto* ep = reinterpret_cast<struct usb_endpoint_descriptor*>(header);
      if (usb_endpoint_type(ep) == USB_ENDPOINT_XFER_BULK) {
        if (usb_endpoint_dir_in(ep)) {
          bulk_in_ep = ep;
        } else {
          bulk_out_ep = ep;
        }
      } else if (usb_endpoint_type(ep) == USB_ENDPOINT_XFER_INT) {
        if (usb_endpoint_dir_in(ep)) {
          intr_in_ep = ep;
        }
      }
    }
  }

  if (!intf || !bulk_out_ep || !bulk_in_ep || !intr_in_ep) {
    fprintf(stderr, "could not find all our endpoints\n");
    return 1;
  }

  ret = usb_device_claim_interface(dev, intf->bInterfaceNumber);
  if (ret < 0) {
    fprintf(stderr, "usb_device_claim_interface failed\n");
  }
  test_interface = intf->bInterfaceNumber;

  cleanup.cancel();

  // Device found, exit from usb_host_load().
  return 1;
}

int usb_device_removed(const char* dev_name, void* client_data) { return 0; }

int usb_discovery_done(void* client_data) { return 0; }

}  // anonymous namespace

int main(int argc, char** argv) {
  // Making command line arguments global to pass to UsbPeripheralConfigurableTests.
  static const struct option long_opts[] = {
      {"time", required_argument, nullptr, 't'},
      {"bytes", required_argument, nullptr, 'b'},
      {"bulk-iterations", required_argument, nullptr, 'i'},
      {"buffer-size", required_argument, nullptr, 's'},
      {"help", no_argument, nullptr, 'h'},
      {"retest", required_argument, nullptr, 'r'},
      {"retest-iterations", required_argument, nullptr, 'n'},
      {0, 0, 0, 0},
  };

  int opt;
  while ((opt = getopt_long_only(argc, argv, "t:b:i:s:r:n:h", long_opts, nullptr)) != -1) {
    switch (opt) {
      case 't':
        UsbPeripheralConfigurableTests::time_ms_ = std::stoi(optarg);
        break;
      case 'b':
        UsbPeripheralConfigurableTests::bytes_ = std::stoi(optarg);
        break;
      case 'i':
        UsbPeripheralConfigurableTests::bulk_iterations_ = std::stoi(optarg);
        break;
      case 's': {
        uint64_t buffer_size = std::stoi(optarg);
        if (buffer_size == 0) {
          fprintf(stderr, "Buffer size is 0, nothing will be transferred. Choose another size.\n");
          return ZX_ERR_INVALID_ARGS;
        }
        UsbPeripheralConfigurableTests::buffer_size_ = buffer_size;
        break;
      }
      case 'r': {
        zx_status_t status = UsbPeripheralConfigurableTests::SetRetestConfigFromString(optarg);
        if (status != ZX_OK) {
          fprintf(
              stderr,
              "Invalid transfer type for usb-peripheral-tests, ZX_STATUS: ZX_ERR_INVALID_ARGS. Expected one of: CONTROL, INTERRUPT, BULK\n");
          return status;
        }
        break;
      }
      case 'n':
        UsbPeripheralConfigurableTests::retest_iterations_ = std::stoi(optarg);
        break;
      case 'h':
        UsbPeripheralConfigurableTests::help_ = true;
        fprintf(stderr, kUsageMsg);
        return 0;
    }
  }

  struct usb_host_context* context = usb_host_init();
  if (!context) {
    fprintf(stderr, "usb_host_context failed\n");
    return -1;
  }
  auto cleanup = fit::defer([context]() {
    usb_host_cleanup(context);
    if (dev) {
      usb_device_close(dev);
    }
  });

  auto ret =
      usb_host_load(context, usb_device_added, usb_device_removed, usb_discovery_done, nullptr);
  if (ret < 0) {
    fprintf(stderr, "usb_host_load failed!\n");
    return -1;
  }
  if (!dev) {
    fprintf(stderr, "No device found, skipping tests.\n");
    return 0;
  }

  // TODO: Add arguments for the zxtest flag options.
  return zxtest::RunAllTests(0, nullptr) ? 0 : -1;
}
