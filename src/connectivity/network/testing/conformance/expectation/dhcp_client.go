// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import "go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"

var dhcpClientExpectations map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 1}:   Pass,
	{2, 1}:   Pass,
	{2, 2}:   Pass,
	{2, 3}:   Pass,
	{4, 1}:   Pass,
	{4, 2}:   Pass,
	{4, 3}:   Pass,
	{4, 4}:   Inconclusive,
	{4, 5}:   Pass,
	{4, 6}:   AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{5, 1}:   Pass,
	{5, 2}:   Fail,
	{5, 3}:   Pass,
	{5, 4}:   Pass,
	{5, 5}:   Pass,
	{5, 6}:   Pass,
	{5, 8}:   Pass,
	{5, 9}:   Pass,
	{5, 10}:  Pass,
	{5, 11}:  Pass,
	{5, 12}:  Fail,
	{5, 13}:  Inconclusive,
	{5, 14}:  Inconclusive,
	{5, 15}:  Inconclusive,
	{6, 1}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 2}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 3}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 4}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 5}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 6}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 7}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 8}:   AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{7, 1}:   AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{8, 1}:   Pass,
	{8, 2}:   Fail,
	{8, 3}:   Pass,
	{8, 4}:   Fail,
	{8, 5}:   Fail,
	{9, 1}:   Pass,
	{10, 1}:  Pass,
	{11, 1}:  Pass,
	{11, 2}:  Pass,
	{11, 3}:  Pass,
	{11, 4}:  Pass,
	{11, 5}:  Fail,
	{11, 6}:  Fail,
	{11, 8}:  Inconclusive,
	{11, 9}:  Inconclusive,
	{11, 10}: Inconclusive,
	{11, 11}: Inconclusive,
	{11, 12}: Inconclusive,
	{11, 13}: Pass,
	{11, 14}: Pass,
	{11, 15}: Pass,
	{12, 1}:  Pass,
	{12, 2}:  Pass,
	{12, 3}:  Fail,
	{12, 4}:  Pass,
	{12, 5}:  Pass,
	{12, 6}:  Pass,
	{12, 7}:  Pass,
	{12, 8}:  Pass,
	{12, 9}:  Pass,
	{12, 10}: Pass,
	{12, 11}: Pass,
	{12, 12}: Pass,
	{13, 1}:  Pass,
	{13, 2}:  Pass,
	{13, 3}:  Pass,
	{13, 4}:  Pass,
	{13, 5}:  Pass,
	{13, 6}:  Pass,
	{13, 7}:  Pass,
	{13, 8}:  Pass,
	// TODO(https://fxbug.dev/42074385): Investigate flake.
	{13, 9}:  Flaky,
	{13, 10}: Inconclusive,
	{14, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{14, 2}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{14, 3}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{15, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{16, 1}:  Pass,
	{16, 2}:  Pass,
	{16, 3}:  Pass,
	{16, 4}:  Pass,
	{16, 5}:  Inconclusive,
	{16, 6}:  Inconclusive,
	{16, 7}:  Pass,
	{16, 8}:  Pass,
	{16, 9}:  Fail,
	{16, 10}: Pass,
}

var dhcpClientExpectationsNS3 map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 1}:  Pass,
	{2, 1}:  Pass,
	{2, 2}:  Pass,
	{2, 3}:  Pass,
	{4, 1}:  Pass,
	{4, 2}:  Pass,
	{4, 3}:  Pass,
	{4, 4}:  Inconclusive,
	{4, 5}:  Pass,
	{4, 6}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{5, 1}:  Pass,
	{5, 2}:  Fail,
	{5, 3}:  Pass,
	{5, 4}:  Pass,
	{5, 5}:  Pass,
	{5, 6}:  Pass,
	{5, 8}:  Pass,
	{5, 9}:  Pass,
	{5, 10}: Pass,
	{5, 11}: Pass,
	{5, 12}: Fail,
	{5, 13}: Inconclusive,
	{5, 14}: Inconclusive,
	{5, 15}: Inconclusive,
	{6, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 2}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 3}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 4}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 5}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 6}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 7}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{6, 8}:  AnvlSkip, // TODO(https://fxbug.dev/42140410): Support forceful lease renewal.
	{7, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{8, 1}:  Pass,
	{8, 2}:  Pass,
	{8, 3}:  Pass,
	{8, 4}:  Fail,
	{8, 5}:  Fail,
	{9, 1}:  Pass,
	{10, 1}: Pass,
	{11, 1}: Pass,
	{11, 2}: Pass,
	{11, 3}: Pass,
	{11, 4}: Pass,
	// Note: 11.5 and 11.6 fail because ANVL does not wait for the DUT to verify
	// the address (i.e. allow DAD to finish), before attempting to use it.
	{11, 5}:  Fail,
	{11, 6}:  Fail,
	{11, 8}:  Inconclusive,
	{11, 9}:  Inconclusive,
	{11, 10}: Inconclusive,
	{11, 11}: Inconclusive,
	{11, 12}: Inconclusive,
	{11, 13}: Pass,
	{11, 14}: Pass,
	{11, 15}: Pass,
	{12, 1}:  Pass,
	{12, 2}:  Pass,
	{12, 3}:  Inconclusive,
	{12, 4}:  Inconclusive,
	{12, 5}:  Inconclusive,
	{12, 6}:  Pass,
	{12, 7}:  Pass,
	{12, 8}:  Pass,
	{12, 9}:  Pass,
	{12, 10}: Pass,
	{12, 11}: Pass,
	{12, 12}: Pass,
	{13, 1}:  Pass,
	{13, 2}:  Pass,
	{13, 3}:  Pass,
	{13, 4}:  Pass,
	{13, 5}:  Pass,
	{13, 6}:  Pass,
	{13, 7}:  Pass,
	{13, 8}:  Pass,
	{13, 9}:  Pass,
	{13, 10}: Inconclusive,
	{14, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{14, 2}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{14, 3}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{15, 1}:  AnvlSkip, // TODO(https://fxbug.dev/42056492): Support DHCPINFORM message.
	{16, 1}:  Pass,
	{16, 2}:  Pass,
	{16, 3}:  Pass,
	{16, 4}:  Pass,
	{16, 5}:  Pass,
	{16, 6}:  Inconclusive,
	{16, 7}:  Pass,
	{16, 8}:  Pass,
	{16, 9}:  Fail,
	{16, 10}: Pass,
}
