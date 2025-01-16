// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package tefmocheck

import (
	"bytes"
	"fmt"
	"path"
	"path/filepath"
	"strings"

	"golang.org/x/exp/slices"

	"go.fuchsia.dev/fuchsia/tools/bootserver/bootserverconstants"
	botanistconstants "go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/build"
	ffxutilconstants "go.fuchsia.dev/fuchsia/tools/lib/ffxutil/constants"
	serialconstants "go.fuchsia.dev/fuchsia/tools/lib/serial/constants"
	syslogconstants "go.fuchsia.dev/fuchsia/tools/lib/syslog/constants"
	netutilconstants "go.fuchsia.dev/fuchsia/tools/net/netutil/constants"
	sshutilconstants "go.fuchsia.dev/fuchsia/tools/net/sshutil/constants"
	"go.fuchsia.dev/fuchsia/tools/testing/runtests"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"
)

// stringInLogCheck checks if String is found in the log named LogName.
type stringInLogCheck struct {
	// String that will be searched for.
	String string
	// OnlyOnStates will cause Check() to return false if the swarming task
	// state doesn't match with one of these states.
	OnlyOnStates []string
	// ExceptStrings will cause Check() to return false if present.
	ExceptStrings []string
	// ExceptBlocks will cause Check() to return false if the string is only
	// within these blocks. The start string and end string should be unique
	// strings that only appear around the except block. A stray start string
	// will cause everything after it to be included in the except block even
	// if the end string is missing.
	ExceptBlocks []*logBlock
	// SkipPassedTask will cause Check() to return false if the
	// Swarming task succeeded.
	SkipPassedTask bool
	// SkipAllPassedTests will cause Check() to return false if all tests
	// in the Swarming task passed.
	SkipAllPassedTests bool
	// SkipPassedTest will cause Check() to return true only if it finds the
	// log in the per-test swarming output of a failed test.
	SkipPassedTest bool
	// IgnoreFlakes will cause Check() to behave in the following ways when
	// combined with other options:
	//   SkipAllPassedTests: Check() will ignore flakes when determining if all
	//     tests passed.
	//   SkipPassedTest: Check() will return the failure as a flake if the
	//     associated test later passes.
	IgnoreFlakes bool
	// AlwaysFlake will always return the failure as a flake so that it doesn't
	// fail the build, but will still be reported as a flake.
	AlwaysFlake bool
	// Type of log that will be checked.
	Type logType
	// Whether to check the per-test Swarming output for this log and emit a
	// check that's specific to the test during which the log appeared.
	AttributeToTest bool
	// If combined with AttributeToTest, it'll list the test name as a tag instead
	// of appending it to the tefmo check name.
	AddTag bool
	// InfraFailure is true if the check is related to infra.
	InfraFailure bool

	swarmingResult *SwarmingRpcsTaskResult
	testName       string
	outputFile     string
	isFlake        bool
	// line that contains the string being searched for. Can be empty.
	line string
}

func (c *stringInLogCheck) Check(to *TestingOutputs) bool {
	c.swarmingResult = to.SwarmingSummary.Results
	if !c.swarmingResult.Failure && c.swarmingResult.State == "COMPLETED" {
		if c.SkipPassedTask {
			return false
		}
		if c.SkipAllPassedTests {
			failedTests := make(map[string]struct{})
			for _, test := range to.TestSummary.Tests {
				if test.Result != runtests.TestSuccess {
					failedTests[test.Name] = struct{}{}
				} else if c.IgnoreFlakes {
					// If a later run of a failed test passed,
					// remove it from the list of failed tests.
					if _, ok := failedTests[test.Name]; ok {
						delete(failedTests, test.Name)
					}
				}
			}
			if len(failedTests) == 0 {
				return false
			}
		}
	}
	matchedState := false
	for _, state := range c.OnlyOnStates {
		if c.swarmingResult.State == state {
			matchedState = true
			break
		}
	}
	if len(c.OnlyOnStates) != 0 && !matchedState {
		return false
	}

	if c.Type == swarmingOutputType && (c.AttributeToTest || c.SkipPassedTest) {
		type testdata struct {
			name       string
			outputFile string
			isFlake    bool
			index      int
			line       string
		}
		failedTestsMap := make(map[string]testdata)
		for i, testLog := range to.SwarmingOutputPerTest {
			var testResult runtests.TestResult
			if to.TestSummary != nil {
				testResult = to.TestSummary.Tests[i].Result
			}
			if c.SkipPassedTest && testResult == runtests.TestSuccess {
				if c.IgnoreFlakes {
					if test, ok := failedTestsMap[testLog.TestName]; ok {
						test.isFlake = true
						failedTestsMap[testLog.TestName] = test
					}
				}
				continue
			}
			found, line := c.checkBytes(to.SwarmingOutput, testLog.Index, testLog.Index+len(testLog.Bytes))
			if found {
				failedTestsMap[testLog.TestName] = testdata{testLog.TestName, testLog.FilePath, c.AlwaysFlake, i, string(line)}
			}
		}
		var failedTests []testdata
		var flakedTests []testdata
		for _, data := range failedTestsMap {
			if data.isFlake {
				flakedTests = append(flakedTests, data)
			} else {
				failedTests = append(failedTests, data)
			}
		}
		// Prioritize returning the first failure. If there are no failures,
		// then return the first flake.
		if len(failedTests) > 0 {
			slices.SortFunc(failedTests, func(a, b testdata) int {
				return a.index - b.index
			})
			if c.AttributeToTest {
				c.testName = failedTests[0].name
				c.outputFile = failedTests[0].outputFile
			}
			return true
		}
		if len(flakedTests) > 0 {
			slices.SortFunc(flakedTests, func(a, b testdata) int {
				return a.index - b.index
			})
			if c.AttributeToTest {
				c.testName = flakedTests[0].name
				c.outputFile = flakedTests[0].outputFile
				c.line = flakedTests[0].line
			}
			c.isFlake = true
			return true
		}
		if c.SkipPassedTest {
			return false
		}
	}

	var toCheck [][]byte
	switch c.Type {
	case serialLogType:
		toCheck = to.SerialLogs
	case swarmingOutputType:
		toCheck = [][]byte{to.SwarmingOutput}
	case syslogType:
		toCheck = to.Syslogs
	}

	for _, file := range toCheck {
		found, line := c.checkBytes(file, 0, len(file))
		if found {
			if c.AlwaysFlake {
				c.isFlake = true
			}
			c.line = string(line)
			return true
		}
	}
	return false
}

func (c *stringInLogCheck) checkBytes(toCheck []byte, start int, end int) (bool, []byte) {
	toCheckBlock := toCheck[start:end]
	for _, s := range c.ExceptStrings {
		if bytes.Contains(toCheckBlock, []byte(s)) {
			return false, nil
		}
	}
	stringBytes := []byte(c.String)
	if len(c.ExceptBlocks) == 0 {
		if idx := bytes.Index(toCheckBlock, stringBytes); idx != -1 {
			line := getLine(toCheck, idx)
			return true, line
		} else {
			return false, nil
		}
	}
	index := bytes.Index(toCheckBlock, stringBytes) + start
	for index >= start && index < end {
		foundString := true
		beforeBlock := toCheck[:index]
		nextStartIndex := index + len(stringBytes)
		afterBlock := toCheck[nextStartIndex:]
		for _, block := range c.ExceptBlocks {
			closestStartIndex := bytes.LastIndex(beforeBlock, []byte(block.startString))
			if closestStartIndex < 0 {
				// There is no start string before this occurrence, so it must not be
				// included in this exceptBlock. Check the next exceptBlock.
				continue
			}
			closestEndIndex := bytes.LastIndex(beforeBlock, []byte(block.endString))
			if closestEndIndex < closestStartIndex {
				// There is no end string between the start string and the string to
				// check, so check if end string appears after. If so, then this
				// occurrence is included in this exceptBlock, so we can break and
				// check the next occurrence of the string.
				if bytes.Contains(afterBlock, []byte(block.endString)) {
					foundString = false
					break
				} else {
					// If the end string doesn't appear after the string to check,
					// it may have been truncated out of the log. In that case, we
					// assume every occurrence of the string to check between the
					// start string and the end of the block are included in the
					// exceptBlock.
					return false, nil
				}
			}
		}
		if foundString {
			line := getLine(toCheck, index)
			return true, line
		}
		index = bytes.Index(afterBlock, stringBytes)
		if index >= 0 {
			index += nextStartIndex
		}
	}
	return false, nil
}

func getLine(toCheck []byte, index int) []byte {
	if index >= len(toCheck) {
		return nil
	}
	lineStart := bytes.LastIndexByte(toCheck[:index], '\n') + 1
	lineEnd := bytes.IndexByte(toCheck[index:], '\n')
	if lineEnd == -1 {
		lineEnd = len(toCheck)
	} else {
		lineEnd += index
	}
	return toCheck[lineStart:lineEnd]
}

func (c *stringInLogCheck) Name() string {
	// TODO(https://fxbug.dev/42150891): With multi-device logs, the file names may be different than
	// the log type. Consider using the actual filename of the log.
	name := path.Join("string_in_log", string(c.Type), strings.ReplaceAll(c.String, " ", "_"))
	if c.testName != "" && !c.AddTag {
		name = path.Join(name, c.testName)
	}
	return name
}

func (c *stringInLogCheck) DebugText() string {
	debugStr := fmt.Sprintf("Found the string \"%s\" in ", c.String)
	if c.outputFile != "" && c.testName != "" {
		debugStr += fmt.Sprintf("%s of test %s.", filepath.Base(c.outputFile), c.testName)
	} else {
		debugStr += fmt.Sprintf("%s for task %s.", c.Type, c.swarmingResult.TaskId)
	}
	debugStr += "\nThat file should be accessible from the build result page or Sponge.\n"
	for _, s := range c.ExceptStrings {
		debugStr += fmt.Sprintf("\nDid not find the exception string \"%s\"", s)
	}
	for _, block := range c.ExceptBlocks {
		debugStr += fmt.Sprintf("\nDid not occur inside a block delimited by:\nSTART: %s\nEND: %s", block.startString, block.endString)
	}
	return debugStr
}

func (c *stringInLogCheck) OutputFiles() []string {
	if c.outputFile == "" {
		return []string{}
	}
	return []string{c.outputFile}
}

func (c *stringInLogCheck) IsFlake() bool {
	return c.isFlake
}

func (c *stringInLogCheck) Tags() []build.TestTag {
	if c.AddTag && c.testName != "" {
		return []build.TestTag{{Key: "test_name", Value: c.testName}}
	}
	return nil
}

func (c *stringInLogCheck) IsInfraFailure() bool {
	return c.InfraFailure
}

func (c *stringInLogCheck) FailureReason() string {
	return c.line
}

// StringInLogsChecks returns checks to detect bad strings in certain logs.
func StringInLogsChecks() []FailureModeCheck {
	ret := []FailureModeCheck{
		// For https://fxbug.dev/42166822
		// This is printed by Swarming after a Swarming task's command completes, and
		// suggests that a test leaked a subprocess that modified one of the task's
		// output files after the task's command completed but before Swarming finished
		// uploading outputs.
		//
		// This is a serious issue and always causes the Swarming task to fail,
		// so we prioritize it over all other checks.
		&stringInLogCheck{String: "error: blob size changed while uploading", Type: swarmingOutputType},
		// Failure modes for CAS uploads from Swarming tasks during task cleanup
		// (outside the scope of the command run during the task). These logs
		// are unfortunately copy-pasted from the luci-go repository. These
		// failures are generally a result of a degradation in the upstream
		// RBE-CAS service.
		&stringInLogCheck{
			String:       "cas: failed to call UploadIfMissing",
			Type:         swarmingOutputType,
			OnlyOnStates: []string{"BOT_DIED"},
		},
		&stringInLogCheck{
			String:       "cas: failed to create cas client",
			Type:         swarmingOutputType,
			OnlyOnStates: []string{"BOT_DIED"},
		},
	}
	// Many of the infra tool checks match failure modes that have a root cause
	// somewhere within Fuchsia itself, so we want to make sure to check for
	// failures within the OS first to make sure we get as close to the root
	// cause as possible.
	ret = append(ret, fuchsiaLogChecks()...)
	ret = append(ret, infraToolLogChecks()...)
	return ret
}

// fuchsiaLogChecks returns checks for logs that come from the target Fuchsia
// device rather than from infrastructure host tools.
func fuchsiaLogChecks() []FailureModeCheck {
	ret := []FailureModeCheck{
		// For https://fxbug.dev/42135406.
		// Hardware watchdog tripped, should not happen.
		// This string is specified in u-boot.
		// Astro uses an equal sign, Sherlock uses a colon. Consider allowing
		// regexes?
		// It is fine to have the two different checks because bug filing logic
		// already breaks down by device type.
		&stringInLogCheck{String: "reboot_mode=watchdog_reboot", Type: serialLogType},
		&stringInLogCheck{String: "reboot_mode:watchdog_reboot", Type: serialLogType},
		// For https://fxbug.dev/42133287
		&stringInLogCheck{String: " in fx_logger::GetSeverity() ", Type: swarmingOutputType},
		// For https://fxbug.dev/42151173. Do not check for this in swarming output as this does not indicate
		// an error if logged by unit tests.
		&stringInLogCheck{String: "intel-display: No displays detected.", Type: serialLogType},
		&stringInLogCheck{String: "intel-display: No displays detected.", Type: syslogType},
		// for https://fxbug.dev/42082278. Broken HDMI emulator on vim3.
		&stringInLogCheck{String: "Failed to parse edid (0 bytes) \"Failed to validate base edid\"", Type: serialLogType},
		&stringInLogCheck{String: "Failed to parse edid (0 bytes) \"Failed to validate base edid\"", Type: syslogType},
		// For https://fxbug.dev/42056605 dwc2 bug that breaks usb cdc networking
		&stringInLogCheck{String: "diepint.timeout", Type: serialLogType, SkipAllPassedTests: true},
		// For devices which, typically as a result of wear, fail to read any copy of the
		// sys_config partition, give up on booting the intended slot, boot the R slot, and
		// severely confuse anyone who was expecting that to be something else.
		// Skip if the task passed; we aim to tolerate failing ECC until we are failing
		// tasks as a result.
		&stringInLogCheck{String: "sys_config: ERROR failed to read any copy", Type: serialLogType, SkipPassedTask: true},
		// Infra expects all devices to be locked when running tasks.  Certain requests may
		// be impossible to fulfill when the device is unlocked because they require secrets
		// which are only available when locked.  If a device is somehow running tasks while
		// unlocked, if the task attempts to make use of those secrets and can't, we'll see
		// this message in the log, and then an infra engineer should go re-lock the device.
		&stringInLogCheck{String: "Please re-lock the device", Type: serialLogType},
		// Certain types of requests on certain devices may be impossible to fulfill when the
		// device has the microphone hardware-muted.  If a device is somehow running tasks
		// with the mute switch in the wrong position, the task may fail and we'll see this
		// message in the log, and then an infra engineer should go flip the mute switch.
		&stringInLogCheck{String: "Mic is muted!  (This would cause all subsequent queries to fail!)", Type: swarmingOutputType},
		// When the DUT reboots due to out-of-memory conditions, pwrbtn-monitor emits this
		// log line.  Since DUTs rebooting during OOM conditions are generally a sign that
		// they won't be running tests as desired, flag that the device rebooted due to OOM.
		// Since we have some host tests which test OOM behavior which print this string,
		// we only consider this to trigger a failure when it appears in serial logs.
		&stringInLogCheck{String: "received kernel OOM signal", Type: serialLogType},
		// For https://fxbug.dev/318087737.
		&stringInLogCheck{
			// LINT.IfChange(blob_header_timeout)
			String: "timed out waiting for http response header while downloading blob",
			// LINT.ThenChange(/src/sys/pkg/bin/pkg-resolver/src/cache.rs:blob_header_timeout)
			Type:               syslogType,
			SkipAllPassedTests: true,
		},
		&stringInLogCheck{
			String: "Got no package for fuchsia-pkg://",
			Type:   swarmingOutputType,
		},
		&stringInLogCheck{
			String: "Exceeded safe temperature range",
			Type:   syslogType,
		},
		&stringInLogCheck{
			// LINT.IfChange(blob_write_failure)
			String: "failed to write blob",
			// LINT.ThenChange(/src/storage/fxfs/platform/src/fuchsia/fxblob/writer.rs:blob_write_failure)
			Type: serialLogType,
		},
		// For https://fxbug.dev/328273002.
		// This detects critical process failures for tests that depend on mexec.
		&stringInLogCheck{
			String: "critical to root job killed with",
			Type:   swarmingOutputType,
			ExceptBlocks: []*logBlock{
				// Mobly tests tend to be e2e tests that sometimes reboot the
				// target, leading to expected job failures.
				{
					startString: "======== Mobly config content ========",
					endString:   "[=====MOBLY RESULTS=====]",
				},
				{
					startString: "=== RUN   TestKillCriticalProcess",
					endString:   "--- PASS: TestKillCriticalProcess",
				},
				{
					startString: "=== RUN   TestOOMHard",
					endString:   "--- PASS: TestOOMHard",
				},
			},
		},
		// For https://fxbug.dev/343550891.
		&stringInLogCheck{
			// LINT.IfChange(watchdog_tefmo)
			String: "has not completed after 30s",
			// LINT.ThenChange(/src/devices/block/drivers/virtio/block.cc:watchdog_tefmo)
			Type:            swarmingOutputType,
			SkipPassedTest:  true,
			IgnoreFlakes:    true,
			AttributeToTest: true,
			AddTag:          true,
		},
	}

	oopsExceptBlocks := []*logBlock{
		{startString: " lock_dep_dynamic_analysis_tests ", endString: " lock_dep_static_analysis_tests "},
		{startString: "RUN   TestKillCriticalProcess", endString: ": TestKillCriticalProcess"},
		{startString: "RUN   TestKernelLockupDetectorCriticalSection", endString: ": TestKernelLockupDetectorCriticalSection"},
		{startString: "RUN   TestKernelLockupDetectorHeartbeat", endString: ": TestKernelLockupDetectorHeartbeat"},
		{startString: "RUN   TestPmmCheckerOops", endString: ": TestPmmCheckerOops"},
		{startString: "RUN   TestKernelLockupDetectorFatalCriticalSection", endString: ": TestKernelLockupDetectorFatalCriticalSection"},
		{startString: "RUN   TestKernelLockupDetectorFatalHeartbeat", endString: ": TestKernelLockupDetectorFatalHeartbeat"},
		// Kernel out-of-memory test "OOMHard" may report valid OOPS that should not reflect a test failure.
		{startString: "RUN   TestOOMHard", endString: ": TestOOMHard"},
		// Suspend e2e test may report a valid OOPS while resuming however the test does not expect resume to succeed.
		// These logs correspond to the beginning and end of the resume logs and do not occur elsewhere in the serial logs.
		{startString: "Restore GNVS pointer", endString: "platform_halt suggested_action 1 reason 6"},
	}
	// These are rather generic. New checks should probably go above here so that they run before these.
	allLogTypes := []logType{serialLogType, swarmingOutputType, syslogType}
	for _, lt := range allLogTypes {
		// For https://fxbug.dev/42119650.
		ret = append(ret, []FailureModeCheck{
			&stringInLogCheck{String: "Timed out loading dynamic linker from fuchsia.ldsvc.Loader", Type: lt},
			&stringInLogCheck{String: "ERROR: AddressSanitizer", Type: lt, AttributeToTest: true, ExceptBlocks: []*logBlock{
				{startString: "[===ASAN EXCEPT BLOCK START===]", endString: "[===ASAN EXCEPT BLOCK END===]"},
			}},
			&stringInLogCheck{String: "ERROR: LeakSanitizer", Type: lt, AttributeToTest: true, ExceptBlocks: []*logBlock{
				// startString and endString should match string in //zircon/system/ulib/c/test/sanitizer/lsan-test.cc.
				{startString: "[===LSAN EXCEPT BLOCK START===]", endString: "[===LSAN EXCEPT BLOCK END===]"},
				// Kernel out-of-memory test "OOMHard" may report false positive leaks.
				{startString: "RUN   TestOOMHard", endString: "PASS: TestOOMHard"},
			}},
			&stringInLogCheck{String: "WARNING: ThreadSanitizer", Type: lt, AttributeToTest: true},
			&stringInLogCheck{String: "SUMMARY: UndefinedBehaviorSanitizer", Type: lt, AttributeToTest: true, ExceptBlocks: []*logBlock{
				{startString: "[===UBSAN EXCEPT BLOCK START===]", endString: "[===UBSAN EXCEPT BLOCK END===]"},
			}},
			// Match specific OOPS types before finally matching the generic type.
			&stringInLogCheck{String: "lockup_detector: no heartbeat from", Type: lt, AttributeToTest: true, ExceptBlocks: oopsExceptBlocks},
			&stringInLogCheck{String: "ZIRCON KERNEL OOPS", Type: lt, AttributeToTest: true, ExceptBlocks: oopsExceptBlocks},
			&stringInLogCheck{String: "ZIRCON KERNEL PANIC", AttributeToTest: true, Type: lt, ExceptBlocks: []*logBlock{
				// These tests intentionally trigger kernel panics.
				{startString: "RUN   TestBasicCrash", endString: "PASS: TestBasicCrash"},
				{startString: "RUN   TestReadUserMemoryViolation", endString: "PASS: TestReadUserMemoryViolation"},
				{startString: "RUN   TestExecuteUserMemoryViolation", endString: "PASS: TestExecuteUserMemoryViolation"},
				{startString: "RUN   TestPmmCheckerPanic", endString: "PASS: TestPmmCheckerPanic"},
				{startString: "RUN   TestCrashAssert", endString: "PASS: TestCrashAssert"},
				{startString: "RUN   TestKernelLockupDetectorFatalCriticalSection", endString: ": TestKernelLockupDetectorFatalCriticalSection"},
				{startString: "RUN   TestKernelLockupDetectorFatalHeartbeat", endString: ": TestKernelLockupDetectorFatalHeartbeat"},
				{startString: "RUN   TestMissingCmdlineEntropyPanics", endString: "PASS: TestMissingCmdlineEntropyPanics"},
				{startString: "RUN   TestIncompleteCmdlineEntropyPanics", endString: "PASS: TestIncompleteCmdlineEntropyPanics"},
				{startString: "RUN   TestDisabledJitterEntropyAndRequiredDoesntBoot", endString: "PASS: TestDisabledJitterEntropyAndRequiredDoesntBoot"},
				{startString: "RUN   TestDisabledJitterEntropyAndRequiredForReseedDoesntReachUserspace", endString: "PASS: TestDisabledJitterEntropyAndRequiredForReseedDoesntReachUserspace"},
			}},
			&stringInLogCheck{String: "double fault, halting", Type: lt},
			// This string can show up in some boot tests.
			&stringInLogCheck{String: "entering panic shell loop", Type: lt, ExceptStrings: []string{"Boot-test-successful!"}},
			// For https://fxbug.dev/42067738. These should track all boot test failures
			// where either the success string shows up in one of the logs but we fail
			// to read it, or the test times out before the success string gets written.
			// LINT.IfChange(seriallistener_timed_out)
			&stringInLogCheck{
				String:             "Boot-test-successful!",
				Type:               lt,
				SkipAllPassedTests: true,
				AlwaysFlake:        true,
				ExceptBlocks: []*logBlock{
					{startString: "seriallistener FATAL: timed out before success string", endString: "was read from serial"},
				},
			},
			&stringInLogCheck{
				String:             "seriallistener FATAL: timed out before success string",
				Type:               lt,
				SkipAllPassedTests: true,
				AlwaysFlake:        true,
			},
			// LINT.ThenChange(/tools/testing/seriallistener/cmd/main.go:timed_out)
		}...)
	}

	// These may be in the output of tests, but the syslog and serial log don't contain any test output.
	for _, lt := range []logType{serialLogType, syslogType} {
		ret = append(ret, []FailureModeCheck{
			// `ASSERT FAILED` may show up in crash hexdumps, so prepend with a space to match it when
			// it's at the start of a logline.
			&stringInLogCheck{String: " ASSERT FAILED", Type: lt, ExceptBlocks: []*logBlock{
				// These tests deliberately trigger asserts in order to verify that they properly terminate
				// the program.
				{startString: "[ RUN      ] ZxAssertCppTest.ZxAssertMsgInvalidPredicateFails", endString: "[       OK ] ZxAssertCppTest.ZxAssertMsgInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCppTest.ZxDebugAssertMsgInvalidPredicateFails", endString: "[       OK ] ZxAssertCppTest.ZxDebugAssertMsgInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCppTest.ZxAssertInvalidPredicateFails", endString: "[       OK ] ZxAssertCppTest.ZxAssertInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCppTest.ZxDebugAssertInvalidPredicateFails", endString: "[       OK ] ZxAssertCppTest.ZxDebugAssertInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCTest.ZxAssertMsgInvalidPredicateFails", endString: "[       OK ] ZxAssertCTest.ZxAssertMsgInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCTest.ZxDebugAssertMsgInvalidPredicateFails", endString: "[       OK ] ZxAssertCTest.ZxDebugAssertMsgInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCTest.ZxAssertInvalidPredicateFails", endString: "[       OK ] ZxAssertCTest.ZxAssertInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxAssertCTest.ZxDebugAssertInvalidPredicateFails", endString: "[       OK ] ZxAssertCTest.ZxDebugAssertInvalidPredicateFails"},
				{startString: "[ RUN      ] ZxTestSmokeTest.DeathStatementCrash", endString: "[       OK ] ZxTestSmokeTest.DeathStatementCrash"},
			}},
			&stringInLogCheck{String: "DEVICE SUSPEND TIMED OUT", Type: lt},
		}...)
	}
	return ret
}

// infraToolLogChecks returns all the checks for logs that are emitted by
// infrastructure host tools.
func infraToolLogChecks() []FailureModeCheck {
	return []FailureModeCheck{
		// For b/291154636
		&stringInLogCheck{String: "Hardware mismatch! Trying to flash images built for", Type: swarmingOutputType},
		// For https://fxbug.dev/42124418.
		&stringInLogCheck{String: "kvm run failed Bad address", Type: swarmingOutputType},
		// For https://fxbug.dev/42121230.
		&stringInLogCheck{String: netutilconstants.CannotFindNodeErrMsg, Type: swarmingOutputType},
		// For https://fxbug.dev/42128160.
		&stringInLogCheck{
			String:         bootserverconstants.FailedToSendErrMsg(bootserverconstants.CmdlineNetsvcName),
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// For https://fxbug.dev/42119464.
		&stringInLogCheck{String: "/dev/net/tun (qemu): Device or resource busy", Type: swarmingOutputType},
		// testrunner logs this when the serial socket goes away unexpectedly.
		&stringInLogCheck{String: ".sock: write: broken pipe", Type: swarmingOutputType},
		// For https://fxbug.dev/42166512.
		&stringInLogCheck{String: "connect: no route to host", Type: swarmingOutputType},
		// For https://fxbug.dev/42135312.
		&stringInLogCheck{
			String: fmt.Sprintf("%s: signal: segmentation fault", botanistconstants.QEMUInvocationErrorMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42079785.
		&stringInLogCheck{
			// LINT.IfChange(fastboot_timeout)
			String: "Timed out while waiting to rediscover device in Fastboot",
			// LINT.ThenChange(/src/developer/ffx/lib/fastboot/src/common/fidl_fastboot_compatibility.rs:fastboot_timeout)
			Type: swarmingOutputType,
		},
		// For https://fxbug.dev/42139740.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToResolveIPErrorMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42143746.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.PackageRepoSetupErrorMsg),
			Type:   swarmingOutputType,
		},
		// For local package server failures.
		&stringInLogCheck{
			String:       fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToServeMsg),
			Type:         swarmingOutputType,
			InfraFailure: true,
		},
		// For failures to resolve packages.
		// LINT.IfChange(tuf_error)
		&stringInLogCheck{
			String:         "rust tuf error",
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// LINT.ThenChange(/src/sys/pkg/bin/pkg-resolver/src/error.rs:tuf_error)
		// For https://fxbug.dev/42143746.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.SerialReadErrorMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42147800.
		&stringInLogCheck{
			String: botanistconstants.FailedToCopyImageMsg,
			Type:   swarmingOutputType,
		},
		// Error is being logged at https://fuchsia.googlesource.com/fuchsia/+/559948a1a4cbd995d765e26c32923ed862589a61/src/storage/lib/paver/paver.cc#175
		&stringInLogCheck{
			String: "Failed to stream partitions to FVM",
			Type:   swarmingOutputType,
			// This error may be emitted, but since we retry paving, this may not be
			// fatal.
			SkipPassedTask: true,
		},
		// Emitted by the GCS Go library during image download.
		&stringInLogCheck{
			String: bootserverconstants.BadCRCErrorMsg,
			Type:   swarmingOutputType,
			// This error is generally transient, so ignore it as long as the
			// download can be retried and eventually succeeds.
			SkipPassedTask: true,
		},
		// For https://fxbug.dev/42170540.
		&stringInLogCheck{
			String: serialconstants.FailedToOpenSerialSocketMsg,
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42170778
		&stringInLogCheck{
			String: serialconstants.FailedToFindCursorMsg,
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42054216. Usually indicates an issue with the bot. If the bots
		// with the failures have been consistently failing with the same error, file
		// a go/fxif-bug for the suspected bad bots.
		&stringInLogCheck{
			String: "server canceled transfer: could not open file for writing",
			Type:   swarmingOutputType,
			// This error may appear as part of a test, so ignore unless it happens
			// during device setup which will cause a task failure.
			SkipPassedTask: true,
		},
		// This error is emitted by `fastboot` when it fails to write an image
		// to the disk. It is generally caused by ECC errors.
		&stringInLogCheck{
			String:       "FAILED (remote: 'error writing the image')",
			Type:         swarmingOutputType,
			InfraFailure: true,
		},
		// For https://fxbug.dev/42178156.
		// This error usually means some kind of USB flakiness/instability when fastboot flashing.
		&stringInLogCheck{
			String: "FAILED (Status read failed (Protocol error))",
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42130478.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToStartTargetMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42128633.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.ReadConfigFileErrorMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42137283.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", sshutilconstants.TimedOutConnectingMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42139705.
		&stringInLogCheck{
			String:       fmt.Sprintf("syslog: %s", syslogconstants.CtxReconnectError),
			Type:         swarmingOutputType,
			OnlyOnStates: []string{"TIMED_OUT"},
		},
		&stringInLogCheck{
			// LINT.IfChange(syslog_failed)
			String: fmt.Sprintf("botanist ERROR: failed to stream syslog multiple times"),
			// LINT.ThenChange(/tools/botanist/targets/target.go:syslog_failed)
			Type: swarmingOutputType,
		},
		&stringInLogCheck{
			String:          fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToCaptureSyslogMsg),
			Type:            swarmingOutputType,
			AlwaysFlake:     true,
			AttributeToTest: true,
			AddTag:          true,
		},
		// For https://fxbug.dev/42130052.
		// Kernel panics and other low-level errors often cause crashes that
		// manifest as SSH failures, so this check must come after all
		// Zircon-related errors to ensure tefmocheck attributes these crashes to
		// the actual root cause.
		&stringInLogCheck{
			String:          fmt.Sprintf("botanist ERROR: %s", testrunnerconstants.FailedToReconnectMsg),
			Type:            swarmingOutputType,
			AttributeToTest: true,
			AddTag:          true,
		},
		// For https://fxbug.dev/42157731.
		&stringInLogCheck{
			String: testrunnerconstants.FailedToStartSerialTestMsg,
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42173783.
		&stringInLogCheck{
			String: ffxutilconstants.TimeoutReachingTargetMsg,
			Type:   swarmingOutputType,
			// ffx might return this error and then succeed connecting to the target
			// on a retry, so we should only check for this failure if ffx fails to
			// connect after all retries and returns a fatal error.
			SkipPassedTask:  true,
			AttributeToTest: true,
			AddTag:          true,
		},
		// For https://fxbug.dev/354707902.
		&stringInLogCheck{
			String:          ffxutilconstants.UnableToResolveAddressMsg,
			Type:            swarmingOutputType,
			SkipPassedTest:  true,
			IgnoreFlakes:    true,
			AttributeToTest: true,
			AddTag:          true,
		},
		// For https://fxbug.dev/321754579.
		&stringInLogCheck{
			String:         fmt.Sprintf("%s: context deadline exceeded", ffxutilconstants.CommandFailedMsg),
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// For https://fxbug.dev/42079078.
		&stringInLogCheck{
			String:         "No daemon was running.",
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// For https://fxbug.dev/42077970.
		&stringInLogCheck{
			String: "FFX Daemon was told not to autostart",
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42083744.
		&stringInLogCheck{
			String:          "Timed out waiting for the ffx daemon on the Overnet mesh over socket",
			Type:            swarmingOutputType,
			AlwaysFlake:     true,
			AttributeToTest: true,
			AddTag:          true,
		},
		&stringInLogCheck{
			String:          "remote command exited without exit status or exit signal",
			Type:            swarmingOutputType,
			AlwaysFlake:     true,
			AttributeToTest: true,
			AddTag:          true,
		},
		// This error happens when `botanist run` exceeds its timeout, e.g.
		// because many tests are taking too long. If botanist exceeds its timeout,
		// it will terminate subprocesses which can lead to the errors below.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.CommandExceededTimeoutMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/42176228.
		&stringInLogCheck{
			String:          ffxutilconstants.ClientChannelClosedMsg,
			Type:            swarmingOutputType,
			SkipPassedTest:  true,
			IgnoreFlakes:    true,
			AttributeToTest: true,
			AddTag:          true,
		},
		// General ffx error check.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist FATAL: %s", ffxutilconstants.CommandFailedMsg),
			Type:   swarmingOutputType,
		},
		&stringInLogCheck{
			String:         fmt.Sprintf("botanist ERROR: %s", ffxutilconstants.CommandFailedMsg),
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// For https://fxbug.dev/42134411.
		// This error usually happens due to an SSH failure, so that error should take precedence.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", testrunnerconstants.FailedToRunSnapshotMsg),
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/338520852 and similar issues.
		// This is caused by lacewing tests which fail to harness the device.
		&stringInLogCheck{
			String: "Termination Signal Type: FuchsiaDeviceError",
			Type:   swarmingOutputType,
		},
		// For https://fxbug.dev/350541791
		// This is most often caused by an ffx daemon delay due to
		// I/O contention on the Cavium
		&stringInLogCheck{
			String:             "Failed to create remote control proxy",
			Type:               swarmingOutputType,
			SkipAllPassedTests: true,
			IgnoreFlakes:       true,
			AttributeToTest:    true,
			AddTag:             true,
		},
		// For https://fxbug.dev/368375861.
		&stringInLogCheck{
			String:      "failed to merge profiles",
			Type:        swarmingOutputType,
			AlwaysFlake: true,
		},
	}
}
