// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import "go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"

var tcpAdvancedV6Expectations map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 17}:  Fail,
	{1, 18}:  Fail,
	{2, 18}:  Fail,
	{2, 19}:  Fail,
	{2, 20}:  Fail,
	{2, 21}:  Fail,
	{2, 22}:  Fail,
	{2, 23}:  Fail,
	{2, 24}:  Fail,
	{2, 25}:  Fail,
	{3, 17}:  Flaky, // TODO(https://fxbug.dev/42056374): Fix the flake.
	{4, 17}:  Flaky,
	{5, 17}:  Fail,
	{5, 18}:  Fail,
	{5, 19}:  Fail,
	{5, 20}:  Fail,
	{5, 21}:  Fail,
	{5, 22}:  Fail,
	{5, 23}:  Fail,
	{6, 17}:  Pass,
	{7, 17}:  Fail,
	{7, 18}:  Fail,
	{8, 17}:  Fail,
	{8, 18}:  Fail,
	{8, 19}:  Fail,
	{8, 20}:  Pass,
	{8, 21}:  Fail,
	{8, 22}:  Fail,
	{8, 23}:  Fail,
	{9, 1}:   AnvlSkip,
	{9, 2}:   AnvlSkip,
	{10, 2}:  AnvlSkip,
	{10, 17}: AnvlSkip,
	{10, 18}: AnvlSkip,
	{10, 19}: AnvlSkip,
	{10, 20}: AnvlSkip,
	{11, 17}: AnvlSkip,
	{12, 2}:  AnvlSkip,
	{12, 17}: AnvlSkip,
	{13, 1}:  AnvlSkip,
	{14, 17}: Skip, // TODO(https://fxbug.dev/400788238): ANVL exits with abnormal code.
	{14, 19}: Skip, // TODO(https://fxbug.dev/400788238): ANVL exits with abnormal code.
	{14, 20}: Fail,
	{15, 17}: Fail,
	{15, 18}: Pass,
	{15, 19}: Pass,
	{15, 20}: Pass,
	{16, 1}:  AnvlSkip,
	{16, 2}:  AnvlSkip,
	{16, 3}:  AnvlSkip,
	{16, 4}:  AnvlSkip,
	{16, 5}:  AnvlSkip,
	{16, 6}:  AnvlSkip,
	{17, 1}:  Pass,
	{17, 17}: Pass,
	{17, 18}: Fail,
	{17, 19}: Pass,
	{17, 20}: Pass,
	{17, 21}: Pass,
	{17, 22}: Pass,
	{17, 23}: Pass,
	{17, 24}: Fail,
}

var tcpAdvancedV6ExpectationsNS3 map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 17}:  Fail,
	{1, 18}:  Fail,
	{2, 18}:  Fail,
	{2, 19}:  Flaky, // TODO(https://fxbug.dev/356692923): This should be a consistent failure.
	{2, 20}:  Fail,
	{2, 21}:  Fail,
	{2, 22}:  Fail,
	{2, 23}:  Fail,
	{2, 24}:  Fail,
	{2, 25}:  Fail,
	{3, 17}:  Pass,
	{4, 17}:  Pass,
	{5, 17}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 18}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 19}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 20}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 21}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 22}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{5, 23}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{6, 17}:  Pass,
	{7, 17}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{7, 18}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 17}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 18}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 19}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 20}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 21}:  Fail,
	{8, 22}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{8, 23}:  Skip, // TODO(https://fxbug.dev/42095226): Implement IP_MTU_DISCOVER to unskip.
	{9, 1}:   AnvlSkip,
	{9, 2}:   AnvlSkip,
	{10, 2}:  AnvlSkip,
	{10, 17}: AnvlSkip,
	{10, 18}: AnvlSkip,
	{10, 19}: AnvlSkip,
	{10, 20}: AnvlSkip,
	{11, 17}: AnvlSkip,
	{12, 2}:  AnvlSkip,
	{12, 17}: AnvlSkip,
	{13, 1}:  AnvlSkip,
	{14, 17}: Skip, // TODO(https://fxbug.dev/400788238): ANVL exits with abnormal code.
	{14, 19}: Pass,
	{14, 20}: Fail,
	{15, 17}: Fail,
	{15, 18}: Pass,
	{15, 19}: Pass,
	{15, 20}: Pass,
	{16, 1}:  AnvlSkip,
	{16, 2}:  AnvlSkip,
	{16, 3}:  AnvlSkip,
	{16, 4}:  AnvlSkip,
	{16, 5}:  AnvlSkip,
	{16, 6}:  AnvlSkip,
	{17, 1}:  Fail,
	{17, 17}: Pass,
	{17, 18}: Fail,
	{17, 19}: Pass,
	{17, 20}: Pass,
	{17, 21}: Pass,
	{17, 22}: Pass,
	{17, 23}: Fail,
	{17, 24}: Fail,
}
