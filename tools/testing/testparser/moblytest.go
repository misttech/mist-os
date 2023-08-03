// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testparser

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"regexp"
	"time"

	"gopkg.in/yaml.v2"

	"go.fuchsia.dev/fuchsia/tools/testing/runtests"
)

const (
	// LINT.IfChange
	moblyTestPreamblePatternStr = `^\[=====MOBLY RESULTS=====\]$`
	// LINT.ThenChange(//src/testing/end_to_end/mobly_driver/api_infra.py)
	moblyTestCaseType = "Record"
)

var moblyTestPreamblePattern = regexp.MustCompile(moblyTestPreamblePatternStr)

type moblyTestCase struct {
	BeginTimeMillis int `yaml:"Begin Time,omitempty"`
	// Description of the cause for test case termination.
	// This is set to empty string for passed test cases.
	Details       string `yaml:"Details,omitempty"`
	EndTimeMillis int    `yaml:"End Time,omitempty"`
	Result        string `yaml:"Result,omitempty"`
	TestClass     string `yaml:"Test Class,omitempty"`
	TestName      string `yaml:"Test Name,omitempty"`
	// Type describes the Mobly YAML document entry type which is in the set of
	// (Record, TestNameList, Summary, ControllerInfo, UserData)
	Type string `yaml:"Type,omitempty"`
}

func parseMoblyTest(lines [][]byte) []runtests.TestCaseResult {
	var res []runtests.TestCaseResult

	if len(lines) < 1 {
		fmt.Fprintf(os.Stderr, "Unexpected Mobly stdout, preamble line missing: %s\n", moblyTestPreamblePatternStr)
		return res
	}

	// Decode YAML document.
	// Skip the first line since it's the non-YAML preamble.
	data := bytes.Join(lines[1:], []byte{'\n'})
	reader := bytes.NewReader(data)
	d := yaml.NewDecoder(reader)

	// Mobly's result YAML file contains multiple YAML documents.
	// Since yaml.Unmarshal() is only capable of parsing a single document,
	// here we use a for-loop and yaml.Decoder() to handle multiple documents.
	for {
		var tc moblyTestCase
		if err := d.Decode(&tc); err != nil {
			// Break the loop at EOF.
			if errors.Is(err, io.EOF) {
				break
			}

			fmt.Fprintf(os.Stderr, "Error unmarshaling YAML: %s\n", err)
			return res
		}

		// Skip records that are not test cases.
		if tc.Type != moblyTestCaseType {
			continue
		}

		var status runtests.TestResult
		switch tc.Result {
		case "PASS":
			status = runtests.TestSuccess
		case "FAIL":
			status = runtests.TestFailure
		case "SKIP":
			status = runtests.TestSkipped
		case "ERROR":
			status = runtests.TestCrashed
		}

		res = append(res, runtests.TestCaseResult{
			DisplayName: fmt.Sprintf("%s.%s", tc.TestClass, tc.TestName),
			FailReason:  tc.Details,
			SuiteName:   tc.TestClass,
			CaseName:    tc.TestName,
			Status:      status,
			Duration:    time.Duration(tc.EndTimeMillis-tc.BeginTimeMillis) * time.Millisecond,
			Format:      "Mobly",
		})
	}
	return res
}
