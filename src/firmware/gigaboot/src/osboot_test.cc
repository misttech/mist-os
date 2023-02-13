// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "osboot.h"

#include <lib/efi/testing/mock_simple_text_input.h>
#include <lib/efi/testing/stub_boot_services.h>
#include <lib/efi/testing/stub_runtime_services.h>
#include <mcheck.h>
#include <stdio.h>
#include <unistd.h>
#include <zircon/boot/image.h>

#include <algorithm>
#include <cstdio>
#include <deque>
#include <fstream>
#include <limits>
#include <optional>
#include <sstream>
#include <string>
#include <vector>

#include <efi/protocol/simple-text-output.h>
#include <gtest/gtest.h>
#include <src/lib/fxl/strings/string_printf.h>

#include "bootbyte.h"
#include "cmdline.h"
#include "gmock/gmock.h"
#include "xefi.h"

namespace {

// Needed to use the ""s operator to embed nulls in string literals.
using namespace std::string_literals;

using ::efi::MatchGuid;
using ::efi::MockBootServices;
using ::efi::MockRuntimeServices;
using ::efi::MockSimpleTextInputProtocol;
using ::efi::StubRuntimeServices;
using ::testing::_;
using ::testing::EndsWith;
using ::testing::Eq;
using ::testing::Ge;
using ::testing::InSequence;
using ::testing::IsEmpty;
using ::testing::MatchesRegex;
using ::testing::NiceMock;
using ::testing::NotNull;
using ::testing::Pointee;
using ::testing::Return;
using ::testing::StrEq;
using ::testing::Test;

const efi_handle kImageHandle = reinterpret_cast<efi_handle>(0x10);
const efi_event kTimerEvent = reinterpret_cast<efi_event>(0x80);

// Bootbyte variable to keep track of the reboot reason in tests.
uint8_t bootbyte = EFI_BOOT_DEFAULT;
char16_t kBootbyteVariableName[] = ZIRCON_BOOTBYTE_EFIVAR;
efi_guid kZirconVendorGuid = ZIRCON_VENDOR_GUID;

// If none of bootbyte, menu, or "bootloader.default" are provided, current
// default is netboot. See if we can switch this to local boot default?
constexpr BootAction kFallthroughBootAction = kBootActionNetboot;

// We don't have efi_simple_test_output_protocol mocks hooked up yet, for now
// just stub them out for simplicity since that's all we need.
EFIAPI efi_status StubEnableCursor(struct efi_simple_text_output_protocol*, bool) {
  return EFI_SUCCESS;
}
EFIAPI efi_status StubSetCursorPosition(struct efi_simple_text_output_protocol*, size_t, size_t) {
  return EFI_SUCCESS;
}

EFIAPI efi_status FakeSetBootbyte(char16_t* name, efi_guid* guid, uint32_t flags, size_t length,
                                  const void* data) {
  EXPECT_EQ(0, memcmp(kBootbyteVariableName, name, sizeof(kBootbyteVariableName)));
  EXPECT_EQ(0, memcmp(&kZirconVendorGuid, guid, sizeof(kZirconVendorGuid)));
  EXPECT_EQ(sizeof(uint8_t), length);
  bootbyte = *static_cast<const uint8_t*>(data);
  return EFI_SUCCESS;
}

EFIAPI efi_status FakeGetBootbyte(char16_t* name, efi_guid* guid, uint32_t* flags, size_t* length,
                                  void* data) {
  EXPECT_EQ(0, memcmp(kBootbyteVariableName, name, sizeof(kBootbyteVariableName)));
  EXPECT_EQ(0, memcmp(&kZirconVendorGuid, guid, sizeof(kZirconVendorGuid)));
  EXPECT_EQ(sizeof(uint8_t), *length);
  *static_cast<uint8_t*>(data) = bootbyte;
  return EFI_SUCCESS;
}

class GetBootActionTest : public Test {
 public:
  void SetUp() override {
    // Configure the necessary mocks for key_prompt().
    system_table_ = efi_system_table{
        .ConIn = mock_input_.protocol(),
        .ConOut = &output_protocol_,
        .BootServices = mock_services_.services(),
    };

    output_protocol_ = efi_simple_text_output_protocol{.SetCursorPosition = StubSetCursorPosition,
                                                       .EnableCursor = StubEnableCursor,
                                                       .Mode = &output_mode_};

    // Just use console in, no need for serial.
    EXPECT_CALL(mock_services_, LocateProtocol(MatchGuid(EFI_SERIAL_IO_PROTOCOL_GUID), _, _))
        .WillOnce(Return(EFI_LOAD_ERROR));

    xefi_init(kImageHandle, &system_table_);

    // Default behavior is to timeout without a key input.
    cmdline_set("bootloader.timeout", "1");
    ON_CALL(mock_input_, ReadKeyStroke).WillByDefault(Return(EFI_NOT_READY));
    ON_CALL(mock_services_, CreateEvent(EVT_TIMER, _, _, _, _))
        .WillByDefault([](uint32_t, efi_tpl, efi_event_notify, void*, efi_event* event) {
          // This doesn't have to point to real memory, but it has to be
          // non-NULL to make it look like the call succeeded.
          *event = kTimerEvent;
          return EFI_SUCCESS;
        });
    ON_CALL(mock_services_, CheckEvent(kTimerEvent)).WillByDefault(Return(EFI_SUCCESS));
  }

  void TearDown() override {
    // Reset all used state in between each test.
    bootbyte = EFI_BOOT_DEFAULT;
    memset(&xefi_global_state, 0, sizeof(xefi_global_state));
    cmdline_clear();
  }

  void SetUserInput(char key) { mock_input_.ExpectReadKeyStroke(key); }

 protected:
  NiceMock<MockBootServices> mock_services_;
  NiceMock<MockSimpleTextInputProtocol> mock_input_;
  efi_simple_text_output_protocol output_protocol_ = {};
  simple_text_output_mode output_mode_ = {};
  efi_system_table system_table_ = {};
  efi_runtime_services mock_runtime_services = efi_runtime_services{
      .GetVariable = FakeGetBootbyte,
      .SetVariable = FakeSetBootbyte,
  };
};

TEST_F(GetBootActionTest, BootbyteRecovery) {
  set_bootbyte(&mock_runtime_services, EFI_BOOT_RECOVERY);
  EXPECT_EQ(kBootActionSlotR, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, BootbyteBootloader) {
  set_bootbyte(&mock_runtime_services, EFI_BOOT_BOOTLOADER);
  EXPECT_EQ(kBootActionFastboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, BootbyteNormal) {
  set_bootbyte(&mock_runtime_services, EFI_BOOT_NORMAL);
  EXPECT_EQ(kFallthroughBootAction, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, BootbyteDefault) {
  set_bootbyte(&mock_runtime_services, EFI_BOOT_DEFAULT);
  EXPECT_EQ(kFallthroughBootAction, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectA) {
  SetUserInput('1');
  EXPECT_EQ(kBootActionSlotA, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectB) {
  SetUserInput('2');
  EXPECT_EQ(kBootActionSlotB, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectRecovery) {
  SetUserInput('r');
  EXPECT_EQ(kBootActionSlotR, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectFastboot) {
  SetUserInput('f');
  EXPECT_EQ(kBootActionFastboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectDfv2) {
  const char* input = "dyes1";
  mock_input_.ExpectReadKeyStrokes(&input);
  bool use_dfv2 = false;
  EXPECT_EQ(kBootActionSlotA, get_boot_action(&mock_runtime_services, true, true, &use_dfv2));
  EXPECT_TRUE(use_dfv2);
}

TEST_F(GetBootActionTest, MenuSelectDfv2Cancelled) {
  const char* input = "dn1";
  mock_input_.ExpectReadKeyStrokes(&input);
  bool use_dfv2 = false;
  EXPECT_EQ(kBootActionSlotA, get_boot_action(&mock_runtime_services, true, true, &use_dfv2));
  EXPECT_FALSE(use_dfv2);
}

TEST_F(GetBootActionTest, MenuSelectDfv2NoUserInput) {
  const efi_event kEventValue = reinterpret_cast<void* const>(0xd00dfeed);
  EXPECT_CALL(mock_services_, CreateEvent)
      .WillRepeatedly([kEventValue](uint32_t type, efi_tpl notify_tpl, efi_event_notify notify_fn,
                                    void* notify_ctx, efi_event* event) {
        *event = kEventValue;
        return EFI_SUCCESS;
      });
  EXPECT_CALL(mock_services_, SetTimer)
      .WillRepeatedly([kEventValue](efi_event event, efi_timer_delay type,
                                    uint64_t trigger_time) -> efi_status {
        if (type != TimerRelative) {
          return EFI_SUCCESS;
        }
        EXPECT_EQ(event, kEventValue);
        EXPECT_GT(trigger_time, 0u);
        return EFI_SUCCESS;
      });
  EXPECT_CALL(mock_services_, CheckEvent).WillRepeatedly([kEventValue](efi_event event) {
    EXPECT_EQ(event, kEventValue);
    return EFI_SUCCESS;
  });

  bool called = false;
  EXPECT_CALL(mock_input_, ReadKeyStroke)
      .WillRepeatedly([&called](efi_input_key* key) -> efi_status {
        if (!called) {
          key->ScanCode = 0;
          key->UnicodeChar = 'd';
          called = true;
          return EFI_SUCCESS;
        }
        return EFI_TIMEOUT;
      });
  bool use_dfv2 = false;
  EXPECT_EQ(kBootActionNetboot, get_boot_action(&mock_runtime_services, true, true, &use_dfv2));
  EXPECT_FALSE(use_dfv2);
}

TEST_F(GetBootActionTest, MenuSelectNetboot) {
  SetUserInput('n');
  EXPECT_EQ(kBootActionNetboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectNetbootRequiresNetwork) {
  // If user tries to select "n" without a network, we should fall through
  // to whatever the bootloader.default commandline arg has.
  cmdline_set("bootloader.default", "local");
  SetUserInput('n');
  EXPECT_EQ(kBootActionDefault, get_boot_action(&mock_runtime_services, false, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineLocal) {
  cmdline_set("bootloader.default", "local");
  EXPECT_EQ(kBootActionDefault, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineNetwork) {
  cmdline_set("bootloader.default", "network");
  EXPECT_EQ(kBootActionNetboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineNetworkRequiresNetwork) {
  // If commandline tries to select network but isn't connected, we should fall
  // back to a boot from disk.
  cmdline_set("bootloader.default", "network");
  EXPECT_EQ(kBootActionDefault, get_boot_action(&mock_runtime_services, false, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineFastboot) {
  cmdline_set("bootloader.default", "fastboot");
  EXPECT_EQ(kBootActionFastboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineZedboot) {
  cmdline_set("bootloader.default", "zedboot");
  EXPECT_EQ(kBootActionSlotR, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineUnknown) {
  // If "bootloader.default" is an unknown value, default to local.
  cmdline_set("bootloader.default", "foo");
  EXPECT_EQ(kBootActionDefault, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineDefault) {
  EXPECT_EQ(kFallthroughBootAction, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, CommandlineDefaultRequiresNetwork) {
  // We only need this while the default action is a netboot, if we change
  // to default to a local boot this test can be deleted.
  static_assert(kBootActionNetboot == kFallthroughBootAction, "Delete this test");

  // If network is unavailable we should fall back to a boot from disk
  // (required for GCE).
  EXPECT_EQ(kBootActionDefault, get_boot_action(&mock_runtime_services, false, true, nullptr));
}

TEST_F(GetBootActionTest, BootbyteFirst) {
  // Make sure the bootbyte is given priority if all are set.
  set_bootbyte(&mock_runtime_services, EFI_BOOT_BOOTLOADER);
  EXPECT_CALL(mock_input_, ReadKeyStroke).Times(0);
  cmdline_set("bootloader.default", "local");
  EXPECT_EQ(kBootActionFastboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

TEST_F(GetBootActionTest, MenuSelectSecond) {
  // Make the user menu is given priority over the commandline.
  SetUserInput('f');
  cmdline_set("bootloader.default", "local");
  EXPECT_EQ(kBootActionFastboot, get_boot_action(&mock_runtime_services, true, true, nullptr));
}

class UefiVariableDumpHelpers : public Test {
 public:
  void SetUp() override {
    // Configure the necessary mocks for memory operation.
    system_table_ = efi_system_table{
        .RuntimeServices = mock_runtime_services_.services(),
        .BootServices = mock_boot_services_.services(),
    };

    xefi_init(kImageHandle, &system_table_);

    ON_CALL(mock_runtime_services_, QueryVariableInfo).WillByDefault(Return(EFI_NOT_FOUND));
    ON_CALL(mock_runtime_services_, GetNextVariableName).WillByDefault(Return(EFI_NOT_FOUND));
    ON_CALL(mock_runtime_services_, GetVariable).WillByDefault(Return(EFI_NOT_FOUND));
  }

  void TearDown() override {}

 protected:
  NiceMock<MockRuntimeServices> mock_runtime_services_;
  NiceMock<MockBootServices> mock_boot_services_;
  efi_system_table system_table_ = {};
};

// Fake printer for test to capture dump output. Class wrapper is
// to ensure that the capture doesn't leak between tests since we
// need to use a global singleton to hook into the raw C function.
class PrintCapture {
 public:
  static std::unique_ptr<PrintCapture> Create() {
    if (PrintCapture::instance_) {
      ADD_FAILURE() << "Can't create 2 PrintCapture objects at a time";
      return nullptr;
    }

    PrintCapture::instance_ = new PrintCapture();
    return std::unique_ptr<PrintCapture>(PrintCapture::instance_);
  }

  ~PrintCapture() { instance_ = nullptr; }

  static int PrintFunction(const char* fmt, ...) {
    if (!instance_) {
      ADD_FAILURE() << "No PrintCapture exists";
      return -1;
    }

    va_list ap;
    va_start(ap, fmt);
    fxl::StringVAppendf(&instance_->contents(), fmt, ap);
    va_end(ap);

    return 0;
  }

  std::string& contents() { return contents_; }

  std::string dump_contents() {
    std::string ret(std::move(contents_));
    contents_.clear();
    return ret;
  }

 private:
  // Private constructor to prevent instantiating 2 at once.
  PrintCapture() = default;

  // Singleton to dispatch calls to.
  static PrintCapture* instance_;

  std::string contents_;
};
PrintCapture* PrintCapture::instance_;

TEST_F(UefiVariableDumpHelpers, UefiPrintHexNullBuffer) {
  auto capture = PrintCapture::Create();

  uefi_var_print_hex(nullptr, 1, PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, UefiPrintHexEmptyBuffer) {
  auto capture = PrintCapture::Create();
  uint8_t buf[0];

  uefi_var_print_hex(buf, 0, PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, UefiPrintHexSymbol) {
  auto capture = PrintCapture::Create();
  const std::pair<uint8_t, std::string> input_and_expected[] = {
      {0x00, "00 "},
      {0x01, "01 "},
      {0x1f, "1f "},
      {0xff, "ff "},
  };

  for (const auto& [input, expected] : input_and_expected) {
    uefi_var_print_hex(&input, 1, PrintCapture::PrintFunction);
    EXPECT_EQ(capture->dump_contents(), expected);
  }
}

TEST_F(UefiVariableDumpHelpers, UefiPrintStrNullBuffer) {
  auto capture = PrintCapture::Create();

  uefi_var_print_str(nullptr, 1, PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, UefiPrintStrEmptyBuffer) {
  auto capture = PrintCapture::Create();
  uint8_t buf[0];

  uefi_var_print_str(buf, 0, PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, UefiPrintStrUtf8Symbols) {
  auto capture = PrintCapture::Create();
  const std::vector<std::string> input = {
      "a",
      "B#C",
      "D,E,F,",
      "abc..def",
  };

  for (auto it : input) {
    uefi_var_print_str((uint8_t*)it.c_str(), it.size(), PrintCapture::PrintFunction);
    EXPECT_THAT(capture->dump_contents(), StrEq(it));
  }
}

TEST_F(UefiVariableDumpHelpers, UefiPrintStrNotprintable) {
  auto capture = PrintCapture::Create();
  const std::pair<std::string, std::string> input_and_expected[] = {
      // clang-format off
      {"\n",            "."},
      {"a\tb",          "a.b"},
      {"\0c\0"s,        ".c."},
      {"\22d\177e\377", ".d.e."},
      // clang-format on
  };

  for (const auto& [input, expected] : input_and_expected) {
    uefi_var_print_str((uint8_t*)input.c_str(), input.size(), PrintCapture::PrintFunction);
    EXPECT_THAT(capture->dump_contents(), StrEq(expected));
  }
}

TEST_F(UefiVariableDumpHelpers, Ucs2LenEmptyString) { EXPECT_EQ(ucs2_len(u"", 2), 0ul); }

TEST_F(UefiVariableDumpHelpers, Ucs2LenEmptyBuffer) { EXPECT_EQ(ucs2_len(u"a", 0), 0ul); }

TEST_F(UefiVariableDumpHelpers, Ucs2LenSmallBuffer) { EXPECT_EQ(ucs2_len(u"", 1), 0ul); }

TEST_F(UefiVariableDumpHelpers, Ucs2LenOneChar) { EXPECT_EQ(ucs2_len(u"a", 2), 1ul); }

TEST_F(UefiVariableDumpHelpers, Ucs2LenOddLength) {
  // pretend buffer has odd length
  EXPECT_EQ(ucs2_len(u"abcdΔ", 11), 5ul);
}

TEST_F(UefiVariableDumpHelpers, Ucs2LenNoNullTermination) {
  EXPECT_EQ(ucs2_len(u"abcdΔ", 10), 5ul);
}

TEST_F(UefiVariableDumpHelpers, Ucs2LenEarlyNullTermination) {
  EXPECT_EQ(ucs2_len(u"ab\0cd", 12), 2ul);
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2EmptyString) {
  auto capture = PrintCapture::Create();

  EXPECT_EQ(print_ucs2(u"", 2, PrintCapture::PrintFunction), 0);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2VisibleChars) {
  auto capture = PrintCapture::Create();
  const char16_t input[] = u"abcDEF123!@#Δ☹☼";
  const std::string expected = "abcDEF123!@#Δ☹☼";

  EXPECT_EQ(print_ucs2(input, sizeof(input), PrintCapture::PrintFunction), 0);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2NotPrintableCodes) {
  auto capture = PrintCapture::Create();
  const char16_t input[] = u"\t\n\x0001\x0002";
  const std::string expected = "\t\n\x01\x02";

  EXPECT_EQ(print_ucs2(input, sizeof(input), PrintCapture::PrintFunction), 0);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2PrintableAndNotPrintableMix) {
  auto capture = PrintCapture::Create();
  const char16_t input[] = u"a\tb\nc\x0001\x0002";
  const std::string expected = "a\tb\nc\x01\x02";

  EXPECT_EQ(print_ucs2(input, sizeof(input), PrintCapture::PrintFunction), 0);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2LongString) {
  auto capture = PrintCapture::Create();
  constexpr size_t CHAR_NUMBER = 4096;
  const std::vector<char16_t> input(CHAR_NUMBER, u'a');
  const std::string expected(CHAR_NUMBER, 'a');

  EXPECT_EQ(print_ucs2(input.data(), input.size() * sizeof(input[0]), PrintCapture::PrintFunction),
            0);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, PrintUcs2MultipleCalls) {
  auto capture = PrintCapture::Create();
  const std::pair<std::u16string, std::string> input_and_expected[] = {
      // clang-format off
      {u"",           ""},
      {u"a",          "a"},
      {u"123",        "123"},
      {u"test\0test", "test"},
      {u"\t\n\x0001", "\t\n\x01"},
      {u"......",     "......"},
      // clang-format on
  };

  for (auto& [input, expected] : input_and_expected) {
    EXPECT_EQ(
        print_ucs2(input.data(), input.size() * sizeof(input[0]), PrintCapture::PrintFunction), 0);
    EXPECT_EQ(capture->dump_contents(), expected);
  }
}

TEST_F(UefiVariableDumpHelpers, UefiPrintVarsNullBuf) {
  auto capture = PrintCapture::Create();

  uefi_print_var(nullptr, 10, PrintCapture::PrintFunction);
  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, UefiPrintVarsValues) {
  auto capture = PrintCapture::Create();
  const std::pair<std::u16string, std::string> input_and_expected[] = {
      // clang-format off
      {u"",           ""},
      {u"a",          "    61 00                                             |a.|\n"},
      {u"test",       "    74 00 65 00 73 00 74 00                           |t.e.s.t.|\n"},
      {u"a.b.c.",     "    61 00 2e 00 62 00 2e 00 63 00 2e 00               |a...b...c...|\n"},
      {u"a\0b",       "    61 00                                             |a.|\n"},
      {u"\t\n\x0001", "    09 00 0a 00 01 00                                 |......|\n"},
      {u"01234567",   "    30 00 31 00 32 00 33 00 34 00 35 00 36 00 37 00   |0.1.2.3.4.5.6.7.|\n"},
      {u"1!@#Δ☹☼",    "    31 00 21 00 40 00 23 00 94 03 39 26 3c 26         |1.!.@.#...9&<&|\n"},
      // clang-format on
  };

  for (auto& [input, expected] : input_and_expected) {
    uefi_print_var((uint8_t*)input.data(), input.size() * sizeof(input[0]),
                   PrintCapture::PrintFunction);
    EXPECT_EQ(capture->dump_contents(), expected);
  }
}

TEST_F(UefiVariableDumpHelpers, UefiPrintVarsMultiline) {
  auto capture = PrintCapture::Create();
  const std::pair<std::u16string, std::string> input_and_expected[] = {
      {u"abcDEF123!@#Δ☹☼",
       "    61 00 62 00 63 00 44 00 45 00 46 00 31 00 32 00   |a.b.c.D.E.F.1.2.|\n"
       "    33 00 21 00 40 00 23 00 94 03 39 26 3c 26         |3.!.@.#...9&<&|\n"},
      {u"0123456789abcdef",
       "    30 00 31 00 32 00 33 00 34 00 35 00 36 00 37 00   |0.1.2.3.4.5.6.7.|\n"
       "    38 00 39 00 61 00 62 00 63 00 64 00 65 00 66 00   |8.9.a.b.c.d.e.f.|\n"},
      {u"0123456789abcdef0",
       "    30 00 31 00 32 00 33 00 34 00 35 00 36 00 37 00   |0.1.2.3.4.5.6.7.|\n"
       "    38 00 39 00 61 00 62 00 63 00 64 00 65 00 66 00   |8.9.a.b.c.d.e.f.|\n"
       "    30 00                                             |0.|\n"},
      {u"0123456789abcdef0123",
       "    30 00 31 00 32 00 33 00 34 00 35 00 36 00 37 00   |0.1.2.3.4.5.6.7.|\n"
       "    38 00 39 00 61 00 62 00 63 00 64 00 65 00 66 00   |8.9.a.b.c.d.e.f.|\n"
       "    30 00 31 00 32 00 33 00                           |0.1.2.3.|\n"},
      {u"0123456789abcdef0123456",
       "    30 00 31 00 32 00 33 00 34 00 35 00 36 00 37 00   |0.1.2.3.4.5.6.7.|\n"
       "    38 00 39 00 61 00 62 00 63 00 64 00 65 00 66 00   |8.9.a.b.c.d.e.f.|\n"
       "    30 00 31 00 32 00 33 00 34 00 35 00 36 00         |0.1.2.3.4.5.6.|\n"},
  };

  for (auto& [input, expected] : input_and_expected) {
    uefi_print_var((uint8_t*)input.data(), input.size() * sizeof(input[0]),
                   PrintCapture::PrintFunction);
    EXPECT_EQ(capture->dump_contents(), expected);
  }
}

TEST_F(UefiVariableDumpHelpers, DumpVarInfoError) {
  auto capture = PrintCapture::Create();

  dump_var_info(PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), IsEmpty());
}

TEST_F(UefiVariableDumpHelpers, DumpVarInfoSuccess) {
  auto capture = PrintCapture::Create();
  const std::string expected(
      "VariableInfo:\n"
      "  Max Storage Size: 1\n"
      "  Remaining Variable Storage Size: 2\n"
      "  Max Variable Size: 3\n");
  EXPECT_CALL(mock_runtime_services_, QueryVariableInfo)
      .WillOnce([](uint32_t attributes, uint64_t* max_var_storage_size,
                   uint64_t* remaining_var_storage_size, uint64_t* max_var_size) {
        *max_var_storage_size = 1;
        *remaining_var_storage_size = 2;
        *max_var_size = 3;
        return EFI_SUCCESS;
      });

  dump_var_info(PrintCapture::PrintFunction);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, DumpVarInfoMaxValues) {
  auto capture = PrintCapture::Create();
  const std::string expected(
      "VariableInfo:\n"
      "  Max Storage Size: 18446744073709551615\n"
      "  Remaining Variable Storage Size: 18446744073709551615\n"
      "  Max Variable Size: 18446744073709551615\n");
  EXPECT_CALL(mock_runtime_services_, QueryVariableInfo)
      .WillOnce([](uint32_t attributes, uint64_t* max_var_storage_size,
                   uint64_t* remaining_var_storage_size, uint64_t* max_var_size) {
        *max_var_storage_size = std::numeric_limits<uint64_t>::max();
        *remaining_var_storage_size = std::numeric_limits<uint64_t>::max();
        *max_var_size = std::numeric_limits<uint64_t>::max();
        return EFI_SUCCESS;
      });

  dump_var_info(PrintCapture::PrintFunction);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, DumpUefiVarsNoVariables) {
  auto capture = PrintCapture::Create();
  auto expected =
      "\n"
      "UEFI variables:\n"
      "\n"
      "No more variables left.\n";

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_EQ(capture->dump_contents(), expected);
}

TEST_F(UefiVariableDumpHelpers, DumpUefiVarsGetNextVarFails) {
  auto capture = PrintCapture::Create();
  const size_t kMinBufSize = 5u;
  auto expected =
      "UEFI variables:\n"
      "\n"
      "No more variables left.\n";

  InSequence s;
  // Ask for bigger buffer
  EXPECT_CALL(mock_runtime_services_, GetNextVariableName(NotNull(), NotNull(), NotNull()))
      .WillOnce([](size_t* var_name_size, char16_t* var_name, efi_guid* vendor_guid) {
        *var_name_size = kMinBufSize;
        return EFI_BUFFER_TOO_SMALL;
      });
  // Fail to provide var with bigger buffer
  EXPECT_CALL(mock_runtime_services_,
              GetNextVariableName(Pointee(Ge(kMinBufSize)), NotNull(), NotNull()))
      .WillOnce(Return(EFI_NOT_FOUND));

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDumpHelpers, DumpUefiVarsGetVarFails) {
  auto capture = PrintCapture::Create();
  auto expected =
      "UEFI variables:\n"
      "test: (0)\n"
      "\n"
      "No more variables left.\n";
  const std::u16string var_name(u"test");
  const size_t kVarNameSize = var_name.size() * sizeof(var_name[0]);
  InSequence s;
  // Ask for bigger buffer
  EXPECT_CALL(mock_runtime_services_, GetNextVariableName(NotNull(), NotNull(), NotNull()))
      .WillOnce([&](size_t* var_name_size, char16_t* var_name, efi_guid* vendor_guid) {
        *var_name_size = kVarNameSize;
        return EFI_BUFFER_TOO_SMALL;
      });

  // Provide var name
  EXPECT_CALL(mock_runtime_services_,
              GetNextVariableName(Pointee(Ge(kVarNameSize)), NotNull(), NotNull()))
      .WillOnce([&](size_t* var_name_size, char16_t* var_name_in, efi_guid* vendor_guid) {
        var_name.copy(var_name_in, var_name.size());
        *var_name_size = kVarNameSize;
        return EFI_SUCCESS;
      });
  // Fail to get variable value
  EXPECT_CALL(mock_runtime_services_, GetVariable(_, _, _, _, _)).WillOnce(Return(EFI_NOT_FOUND));
  EXPECT_CALL(mock_runtime_services_,
              GetNextVariableName(Pointee(Ge(kVarNameSize)), NotNull(), NotNull()))
      .WillOnce(Return(EFI_NOT_FOUND));

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDumpHelpers, DumpUefiVarsGetOneVariable) {
  auto capture = PrintCapture::Create();
  const std::u16string var_name(u"test");
  const size_t kVarNameSize = var_name.size() * sizeof(var_name[0]);
  const std::vector<uint8_t> var_val({0x00, 0x01, 0x02, 0x03, 'a', 'b', 'c', 'd'});
  InSequence s;
  // Ask for bigger buffer for var_name
  EXPECT_CALL(mock_runtime_services_, GetNextVariableName(NotNull(), NotNull(), NotNull()))
      .WillOnce([&](size_t* var_name_size, char16_t* var_name, efi_guid* vendor_guid) {
        *var_name_size = kVarNameSize;
        return EFI_BUFFER_TOO_SMALL;
      });
  // Provide var_name
  EXPECT_CALL(mock_runtime_services_,
              GetNextVariableName(Pointee(Ge(kVarNameSize)), NotNull(), NotNull()))
      .WillOnce([&](size_t* var_name_size, char16_t* var_name_in, efi_guid* vendor_guid) {
        var_name.copy(var_name_in, var_name.size());
        return EFI_SUCCESS;
      });
  // Ask for bigger buffer for var value
  EXPECT_CALL(mock_runtime_services_, GetVariable(_, _, _, _, _))
      .WillOnce([&](char16_t* var_name, efi_guid* vendor_guid, uint32_t* attributes,
                    size_t* data_size, void* data) {
        *data_size = var_val.size();
        return EFI_BUFFER_TOO_SMALL;
      });
  // Provide var value
  EXPECT_CALL(mock_runtime_services_, GetVariable(_, _, _, _, _))
      .WillOnce([&](char16_t* var_name, efi_guid* vendor_guid, uint32_t* attributes,
                    size_t* data_size, void* data) {
        std::copy_n(var_val.begin(), var_val.size(), (uint8_t*)data);
        return EFI_SUCCESS;
      });
  // Indicate last variable was read
  EXPECT_CALL(mock_runtime_services_, GetNextVariableName).WillOnce(Return(EFI_NOT_FOUND));

  auto expected =
      "UEFI variables:\n"
      "test: (8)\n"
      "    00 01 02 03 61 62 63 64                           |....abcd|\n"
      "\n"
      "No more variables left.\n";

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

class UefiVariableDump : public Test {
 public:
  void SetUp() override {
    // Configure the necessary mocks for memory operation.
    system_table_ = efi_system_table{
        .RuntimeServices = stub_runtime_services_.services(),
        .BootServices = mock_boot_services_.services(),
    };

    xefi_init(kImageHandle, &system_table_);
  }

  void TearDown() override {}

  void SetVariables(const std::list<std::pair<StubRuntimeServices::VariableName,
                                              StubRuntimeServices::VariableValue>>& vars) {
    stub_runtime_services_.SetVariables(vars);
  }

 protected:
  StubRuntimeServices stub_runtime_services_;
  NiceMock<MockBootServices> mock_boot_services_;
  efi_system_table system_table_ = {};
};

TEST_F(UefiVariableDump, DumpVarInfo) {
  auto capture = PrintCapture::Create();
  const std::string expected(
      "VariableInfo:\n"
      "  Max Storage Size: [0-9]*\n"
      "  Remaining Variable Storage Size: [0-9]*\n"
      "  Max Variable Size: [0-9]*\n");

  dump_var_info(PrintCapture::PrintFunction);

  EXPECT_THAT(capture->dump_contents(), MatchesRegex(expected));
}

TEST_F(UefiVariableDump, EmptyVar) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {{u"VarName", {}}, {}},
      };
  const std::string expected =
      "UEFI variables:\n"
      "VarName: (0)\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultiVarSameSize) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {{u"VarName0", {}}, {0x00}},
          {{u"VarName1", {}}, {0x01}},
          {{u"VarName2", {}}, {0x02}},
      };
  const std::string expected =
      "UEFI variables:\n"
      "VarName0: (1)\n"
      "    00                                                |.|\n"
      "VarName1: (1)\n"
      "    01                                                |.|\n"
      "VarName2: (1)\n"
      "    02                                                |.|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultiVarSmallerSize) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {{u"VarName0", {}}, {0x00}},
          {{u"VarNam1", {}}, {0x01}},
          {{u"VarNa2", {}}, {0x02}},
          {{u"VarN3", {}}, {0x03}},
      };
  const std::string expected =
      "UEFI variables:\n"
      "VarName0: (1)\n"
      "    00                                                |.|\n"
      "VarNam1: (1)\n"
      "    01                                                |.|\n"
      "VarNa2: (1)\n"
      "    02                                                |.|\n"
      "VarN3: (1)\n"
      "    03                                                |.|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultiVarBiggerSize) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {{u"VarN3", {}}, {0x03}},
          {{u"VarNa2", {}}, {0x02}},
          {{u"VarNam1", {}}, {0x01}},
          {{u"VarName0", {}}, {0x00}},
      };
  const std::string expected =
      "UEFI variables:\n"
      "VarN3: (1)\n"
      "    03                                                |.|\n"
      "VarNa2: (1)\n"
      "    02                                                |.|\n"
      "VarNam1: (1)\n"
      "    01                                                |.|\n"
      "VarName0: (1)\n"
      "    00                                                |.|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultiVarMixedSize) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {{u"VarName0", {}}, {0x00}},
          {{u"VarNa2", {}}, {0x02}},
          {{u"VarNam1", {}}, {0x01}},
          {{u"VarN3", {}}, {0x03}},
      };
  const std::string expected =
      "UEFI variables:\n"
      "VarName0: (1)\n"
      "    00                                                |.|\n"
      "VarNa2: (1)\n"
      "    02                                                |.|\n"
      "VarNam1: (1)\n"
      "    01                                                |.|\n"
      "VarN3: (1)\n"
      "    03                                                |.|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultilineValues) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {
              {u"Var1Byte", {}},
              {0x00},
          },
          {
              {u"Var15Bytes", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e},
          },
          {
              {u"Var16BytesSingleLine", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e, 0x0f},
          },
          {
              {u"Var17Bytes2Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e, 0x0f, 0x10},
          },
          {
              {u"Var63Bytes4Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
               0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
               0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
               0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33,
               0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e},
          },
          {
              {u"Var64Bytes4Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
               0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
               0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
               0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33,
               0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f},
          },
      };
  const std::string expected =
      "UEFI variables:\n"
      "Var1Byte: (1)\n"
      "    00                                                |.|\n"
      "Var15Bytes: (15)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e      |...............|\n"
      "Var16BytesSingleLine: (16)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "Var17Bytes2Lines: (17)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "    10                                                |.|\n"
      "Var63Bytes4Lines: (63)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "    10 11 12 13 14 15 16 17 18 19 1a 1b 1c 1d 1e 1f   |................|\n"
      "    20 21 22 23 24 25 26 27 28 29 2a 2b 2c 2d 2e 2f   | !\"#$%&'()*+,-./|\n"
      "    30 31 32 33 34 35 36 37 38 39 3a 3b 3c 3d 3e      |0123456789:;<=>|\n"
      "Var64Bytes4Lines: (64)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "    10 11 12 13 14 15 16 17 18 19 1a 1b 1c 1d 1e 1f   |................|\n"
      "    20 21 22 23 24 25 26 27 28 29 2a 2b 2c 2d 2e 2f   | !\"#$%&'()*+,-./|\n"
      "    30 31 32 33 34 35 36 37 38 39 3a 3b 3c 3d 3e 3f   |0123456789:;<=>?|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityFull);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, MultilineValuesShort) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {
          {
              {u"Var1Byte", {}},
              {0x00},
          },
          {
              {u"Var15Bytes", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e},
          },
          {
              {u"Var16BytesSingleLine", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e, 0x0f},
          },
          {
              {u"Var17Bytes2Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
               0x0e, 0x0f, 0x10},
          },
          {
              {u"Var63Bytes4Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
               0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
               0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
               0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33,
               0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e},
          },
          {
              {u"Var64Bytes4Lines", {}},
              {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
               0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
               0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26,
               0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33,
               0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f},
          },
      };
  const std::string expected =
      "UEFI variables:\n"
      "Var1Byte: (1)\n"
      "    00                                                |.|\n"
      "Var15Bytes: (15)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e      |...............|\n"
      "Var16BytesSingleLine: (16)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "Var17Bytes2Lines: (17)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "Var63Bytes4Lines: (63)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "Var64Bytes4Lines: (64)\n"
      "    00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f   |................|\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityShort);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

TEST_F(UefiVariableDump, DumpVarNames) {
  auto capture = PrintCapture::Create();
  const std::list<std::pair<StubRuntimeServices::VariableName, StubRuntimeServices::VariableValue>>
      vars = {{{u"V", {}}, {0x00}},
              {{u"VarShort", {}}, {0x00}},
              {{u"VV", {}}, {0x00}},
              {{u"MuchLongerVariableName", {}}, {0x00}},
              {{u"VarWithFunnyCharsInName_!@#Δ☹☼", {}}, {0x00}}};
  const std::string expected =
      "UEFI variables:\n"
      "V: (1)\n"
      "VarShort: (1)\n"
      "VV: (1)\n"
      "MuchLongerVariableName: (1)\n"
      "VarWithFunnyCharsInName_!@#Δ☹☼: (1)\n"
      "\n"
      "No more variables left.\n";

  SetVariables(vars);

  dump_uefi_vars_custom_printer(PrintCapture::PrintFunction, kEfiVarDumpVerbosityNameOnly);

  EXPECT_THAT(capture->dump_contents(), EndsWith(expected));
}

}  // namespace
