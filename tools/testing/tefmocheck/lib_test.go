// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package tefmocheck

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/testing/runtests"

	"github.com/google/go-cmp/cmp"
)

func TestLoadTestSummaryPassesInputSummaryThrough(t *testing.T) {
	inputSummary := runtests.TestSummary{
		Tests: []runtests.TestDetails{
			{Name: "test name"},
		},
	}
	summaryBytes, err := json.Marshal(inputSummary)
	if err != nil {
		t.Fatal("Marshal(inputSummary) failed:", err)
	}
	outputSummary, err := LoadTestSummary(mkTempFile(t, summaryBytes))
	if err != nil {
		t.Errorf("LoadSwarmingTaskSummary failed: %v", err)
	} else if diff := cmp.Diff(outputSummary, &inputSummary); diff != "" {
		t.Errorf("LoadSwarmingTaskSummary returned wrong value (-got +want):\n%s", diff)
	}
}

// mkTempFile returns a new temporary file containing the specified content
// that will be cleaned up automatically.
func mkTempFile(t *testing.T, content []byte) string {
	name := filepath.Join(t.TempDir(), "tefmocheck-cmd-test")
	if err := os.WriteFile(name, content, 0o600); err != nil {
		t.Fatal(err)
	}
	return name
}

func TestIgnoreTestsForFlakeAnalysis(t *testing.T) {
	summary := runtests.TestSummary{
		Tests: []runtests.TestDetails{
			{Name: "test 1", Result: runtests.TestFailure},
			{Name: "test 2", Result: runtests.TestAborted},
			{Name: "test 3", Result: runtests.TestAborted},
			{Name: "test 4", Result: runtests.TestAborted},
			{Name: "test 5", Result: runtests.TestFailure},
			{Name: "test 6", Result: runtests.TestInfraFailure},
		},
	}
	testCases := []struct {
		name                 string
		summary              runtests.TestSummary
		checks               []FailureModeCheck
		hasFailingTefmocheck bool
	}{
		{
			name:                 "fuchsia_analysis_ignore tag is added correctly",
			summary:              summary,
			checks:               append([]FailureModeCheck{}, MassTestFailureCheck{MaxFailed: 5}),
			hasFailingTefmocheck: true,
		},
		{
			name:                 "does not add tags if there are no tefmochecks",
			summary:              summary,
			checks:               []FailureModeCheck{},
			hasFailingTefmocheck: false,
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			to := TestingOutputs{
				TestSummary: &summary,
			}
			outputsDir := ""
			checkTests, err := RunChecks(tc.checks, &to, outputsDir)
			if err != nil {
				t.Fatalf("failed to run checks: %v", err)
			}
			if len(checkTests) == 0 && tc.hasFailingTefmocheck {
				t.Fatalf("expected a MassTestFailureCheck")
			}

			IgnoreTestsForFlakeAnalysis(tc.summary.Tests, checkTests)

			for _, test := range summary.Tests {
				hasFlakeAnalysisIgnoreTag := false
				isFailure := runtests.IsFailure(test.Result)
				for _, tag := range test.Tags {
					if tag.Key == "flake_analysis_ignore" && tag.Value == "true" {
						hasFlakeAnalysisIgnoreTag = true
					}
				}
				if isFailure && !hasFlakeAnalysisIgnoreTag {
					t.Errorf("IgnoreTestsForFlakeAnalysis did not add a flake_analysis_ignore tag to a test failure: %s", test.Name)
				}
				if !isFailure && hasFlakeAnalysisIgnoreTag {
					t.Errorf("IgnoreTestsForFlakeAnalysis unexpectedly added a flake_analysis_ignore tag to a passing or skipped test: %s", test.Name)
				}
			}
		})
	}
}
