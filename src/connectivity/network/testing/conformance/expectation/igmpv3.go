// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import (
	"go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"
)

var igmpv3ExpectationsNS3 map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 2}: Pass,
	{2, 1}: Pass,
	{2, 2}: Pass,
	{2, 3}: Inconclusive,
	{2, 4}: Inconclusive,
	{3, 1}: Inconclusive,
	{3, 2}: Inconclusive,
	{4, 2}: Pass,
	{4, 3}: Pass,
	{4, 6}: Pass,
	{4, 7}: Pass,
	// Discard messages with bad TOS. Feels too strict, not explicitly called
	// out in RFC.
	{4, 10}: Fail,
	{4, 11}: Pass,
	{4, 14}: Pass,
	{4, 15}: Pass,
	{4, 20}: Pass,
	{5, 1}:  Pass,
	{5, 2}:  Pass,
	{5, 3}:  Pass,
	{5, 5}:  Pass,
	{5, 10}: Pass,
	{5, 24}: Pass,
	{5, 25}: Pass,
	// At odds with test 11.1. Both tests generate the same input. 5.31 expects
	// an answer, 11.1 expects the query to be ignored. The RFC itself is
	// ambiguous about this, but we side with 11.1.
	{5, 31}: Fail,
	{5, 32}: Pass,
	{6, 1}:  Pass,
	{6, 3}:  Pass,
	{6, 5}:  Pass,
	{6, 6}:  Inconclusive,
	{6, 7}:  Pass,
	{6, 8}:  Inconclusive,
	{6, 9}:  Pass,
	{6, 11}: Pass,
	{6, 14}: Inconclusive,
	{6, 15}: Fail,
	{6, 16}: Inconclusive,
	{6, 17}: Fail,
	{6, 18}: Inconclusive,
	{6, 19}: Inconclusive,
	{6, 20}: Inconclusive,
	{6, 21}: Inconclusive,
	{6, 22}: Inconclusive,
	{6, 23}: Pass,
	{6, 25}: Pass,
	{6, 27}: Pass,
	{6, 28}: Pass,
	{8, 2}:  Pass,
	{8, 3}:  Pass,
	{8, 4}:  Inconclusive,
	{8, 5}:  Inconclusive,
	{8, 6}:  Inconclusive,
	{8, 7}:  Inconclusive,
	{8, 8}:  Inconclusive,
	{8, 9}:  Inconclusive,
	{8, 10}: Inconclusive,
	{8, 11}: Inconclusive,
	{8, 12}: Fail,
	{8, 13}: Inconclusive,
	{8, 14}: Inconclusive,
	{8, 15}: Fail,
	{8, 16}: Pass,
	{8, 17}: Inconclusive,
	{10, 4}: Pass,
	{10, 5}: Pass,
	{10, 6}: Pass,
	{10, 7}: Pass,
	// TODO(https://fxbug.dev/384382023): Test definition seems to be using
	// wrong timeout which causes this to be flaky.
	{10, 8}:  Flaky,
	{10, 9}:  Pass,
	{10, 10}: Pass,
	{10, 11}: Pass,
	{10, 12}: Pass,
	{10, 13}: Pass,
	{10, 14}: Pass,
	{10, 15}: Fail,
	{10, 16}: Fail,
	{11, 1}:  Pass,
}
