// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import (
	"os"
	"testing"

	"go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/platform"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/parseoutput"
)

func TestGetExpectation(t *testing.T) {
	const (
		SuiteName string = "IP"
		// Use IP-5.6 since the expected outcome differs between stacks.
		MajorNumber        int = 5
		MinorNumber        int = 6
		MissingMinorNumber int = 2
	)
	var platforms = []struct {
		name           string
		plt            platform.Platform
		expectedResult outcome.Outcome
	}{
		{
			name:           "ns2",
			plt:            platform.NS2,
			expectedResult: outcome.Pass,
		},
		{
			name:           "ns3",
			plt:            platform.NS3,
			expectedResult: outcome.Fail,
		},
	}
	for _, platform := range platforms {
		t.Run(platform.name, func(t *testing.T) {
			result, ok := GetExpectation(parseoutput.CaseIdentifier{platform.plt.String(), SuiteName, MajorNumber, MinorNumber})
			if !ok {
				t.Fatalf("expectation missing for %s %d.%d", SuiteName, MajorNumber, MinorNumber)
			}
			if result != platform.expectedResult {
				t.Errorf("wrong expectation for %s %d.%d: got = %s, want = %s", SuiteName, MajorNumber, MinorNumber, result, platform.expectedResult)
			}

			os.Setenv("ANVL_DEFAULT_EXPECTATION_PASS", "true")
			result, ok = GetExpectation(parseoutput.CaseIdentifier{platform.plt.String(), SuiteName, MajorNumber, MissingMinorNumber})
			if !ok {
				t.Fatalf("expectation missing for %s %d.%d", SuiteName, MajorNumber, MissingMinorNumber)
			}
			if result != outcome.Pass {
				t.Errorf("wrong expectation for %s %d.%d: got = %s, want = %s", SuiteName, MajorNumber, MissingMinorNumber, result, outcome.Pass)
			}
		})
	}
}
