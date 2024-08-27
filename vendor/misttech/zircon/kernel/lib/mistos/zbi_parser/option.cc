// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_OPTION_CC_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_OPTION_CC_

#include "lib/mistos/zbi_parser/option.h"

#include <lib/boot-options/word-view.h>
#include <lib/stdcompat/string_view.h>
#include <trace.h>

#include <ktl/string_view.h>
#include <ktl/algorithm.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {

constexpr ktl::string_view kOptPrefix = "userboot";
constexpr ktl::string_view kRootOpt = "userboot.root";
constexpr ktl::string_view kNextOpt = "userboot.next";
constexpr ktl::string_view kTestRootOpt = "userboot.test.root";
constexpr ktl::string_view kTestNextOpt = "userboot.test.next";

// TODO(joshuaseaton): This should really be defined as a default value of
// `Options::next` and expressed as a std::string_view; however, that can
// sometimes generate a writable data section. While such sections are
// prohibited, we apply the default within ParseCmdline() below and keep this
// value as a char array.
constexpr const char kNextDefault[] = "bin/init";

struct KeyAndValue {
  ktl::string_view key, value;
};

KeyAndValue SplitOpt(ktl::string_view opt) {
  ktl::string_view key = opt.substr(0, opt.find('='));
  opt.remove_prefix(ktl::min(opt.size(), key.size() + 1));
  return {key, opt};
}

constexpr ktl::string_view NormalizePath(ktl::string_view view) {
  if (view.empty() || view.back() != '/') {
    return view;
  }
  return view.substr(0, view.length() - 1);
}

constexpr bool ParseOption(ktl::string_view key, ktl::string_view value,
                           zbi_parser::Options& opts) {
  if (key == kNextOpt) {
    opts.boot.next = value;
    return true;
  }

  if (key == kRootOpt) {
    opts.boot.root = NormalizePath(value);
    return true;
  }

  if (key == kTestNextOpt) {
    opts.test.next = value;
    return true;
  }

  if (key == kTestRootOpt) {
    opts.test.root = NormalizePath(value);
    return true;
  }

  return false;
}

}  // namespace

namespace zbi_parser {

void ParseCmdline(ktl::string_view cmdline, Options& opts) {
  for (ktl::string_view opt : WordView(cmdline)) {
    if (!ktl::starts_with(opt, kOptPrefix)) {
      continue;
    }

    auto [key, value] = SplitOpt(opt);
    if (ParseOption(key, value, opts)) {
      LTRACEF_LEVEL(2, "OPTION %.*s%s%.*s\n", static_cast<int>(key.size()), key.data(),
                    value.empty() ? "" : "=", static_cast<int>(value.size()), value.data());
    } else {
      LTRACEF_LEVEL(2, "WARNING: unknown option %.*s ignored\n", static_cast<int>(key.size()),
                    key.data());
    }
  }

  // Only set default boot program for non test environments.
  if (opts.boot.next.empty() && opts.test.next.empty()) {
    opts.boot.next = kNextDefault;
  }

  if (!opts.boot.root.empty() && opts.boot.root.front() == '/') {
    TRACEF("`userboot.root` (\"%.*s\" must not begin with a \'/\'",
           static_cast<int>(opts.boot.root.size()), opts.boot.root.data());
  }
}

}  // namespace zbi_parser

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_OPTION_CC_
