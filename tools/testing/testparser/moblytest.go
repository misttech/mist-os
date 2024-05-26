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
	moblyTestPreamblePatternStr         = `^======== Mobly config content ========$`
	moblyTestResultYAMLHeaderPatternStr = `^\[=====MOBLY RESULTS=====\]$`
	moblyTestCaseType                   = "Record"
	moblySummaryType                    = "Summary"
)

var (
	moblyTestPreamblePattern         = regexp.MustCompile(moblyTestPreamblePatternStr)
	moblyTestResultYAMLHeaderPattern = regexp.MustCompile(moblyTestResultYAMLHeaderPatternStr)
)

type moblyTestCase struct {
	BeginTimeMillis int `yaml:"Begin Time"`
	// Description of the cause for test case termination.
	// This is set to empty string for passed test cases.
	Details       string `yaml:"Details"`
	EndTimeMillis int    `yaml:"End Time"`
	Result        string `yaml:"Result"`
	TestClass     string `yaml:"Test Class"`
	TestName      string `yaml:"Test Name"`
	// TerminationSignal is name of the Python exception class raised on crash/failure.
	TerminationSignal string `yaml:"Termination Signal Type"`
	// Type describes the Mobly YAML document entry type which is in the set of
	// (Record, TestNameList, Summary, ControllerInfo, UserData)
	Type string `yaml:"Type"`
}

func matchYAMLHeader(lines [][]byte) [][]byte {
	for num, line := range lines {
		if moblyTestResultYAMLHeaderPattern.Match(line) {
			// Skip the matched YAML header line.
			return lines[num+1:]
		}
	}
	fmt.Fprintf(os.Stderr, "Unepxected Mobly output, Mobly result YAML header line missing: %s\n", moblyTestResultYAMLHeaderPatternStr)
	return nil
}

func parseMoblyTest(lines [][]byte) []runtests.TestCaseResult {
	var res []runtests.TestCaseResult

	if len(lines) < 1 {
		fmt.Fprintf(os.Stderr, "Unexpected Mobly stdout, preamble line missing: %s\n", moblyTestPreamblePatternStr)
		return res
	}

	// Find YAML header and decode YAML document.
	yamlLines := matchYAMLHeader(lines)
	data := bytes.Join(yamlLines, []byte{'\n'})
	reader := bytes.NewReader(data)
	d := yaml.NewDecoder(reader)

	// Mobly reports a "summary" record at the end of the YAML document. Its
	// presence means that the Mobly test ran to completion.
	summarySeen := false

	// Since yaml.Unmarshal() is only capable of parsing a single document,
	// we use a for-loop and yaml.Decoder() to handle the remaining documents.
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
			if tc.Type == moblySummaryType {
				summarySeen = true
			}
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

		failureReason := tc.Details
		if len(tc.TerminationSignal) != 0 {
			failureReason = fmt.Sprintf("[%s] %s", tc.TerminationSignal, failureReason)
		}

		res = append(res, runtests.TestCaseResult{
			DisplayName: fmt.Sprintf("%s.%s", tc.TestClass, tc.TestName),
			FailReason:  failureReason,
			SuiteName:   tc.TestClass,
			CaseName:    tc.TestName,
			Status:      status,
			Duration:    time.Duration(tc.EndTimeMillis-tc.BeginTimeMillis) * time.Millisecond,
			Format:      "Mobly",
		})
	}

	if !summarySeen {
		// Generate a synthetic result if the summary record is not seen which
		// is indicative of an infra test timeout.
		res = append(res, runtests.TestCaseResult{
			DisplayName: "TestparserError",
			FailReason:  fmt.Sprintf("[TestparserError] Missing Mobly summary record - potental infra timeout."),
			SuiteName:   "Synthetic",
			CaseName:    "Synthetic",
			Status:      runtests.TestAborted,
			Format:      "Mobly",
		})
	}
	return res
}
