// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testrunner

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	resultpb "go.chromium.org/luci/resultdb/proto/v1"
	sinkpb "go.chromium.org/luci/resultdb/sink/proto/v1"
	botanist "go.fuchsia.dev/fuchsia/tools/botanist"
	botanistconstants "go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/botanist/targets"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder"
	"go.fuchsia.dev/fuchsia/tools/lib/clock"
	"go.fuchsia.dev/fuchsia/tools/lib/environment"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
	"go.fuchsia.dev/fuchsia/tools/testing/resultdb"
	"go.fuchsia.dev/fuchsia/tools/testing/runtests"
	"go.fuchsia.dev/fuchsia/tools/testing/tap"
	"go.fuchsia.dev/fuchsia/tools/testing/testparser"
	"go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"
)

const testTimeoutGracePeriod = 30 * time.Second

type Options struct {
	// The path where a directory containing test results should be created.
	OutDir string

	// Working directory of the local testing subprocesses.
	LocalWD string

	// The path to an NsJail binary.
	NsjailPath string

	// The path to mount as NsJail's root directory.
	NsjailRoot string

	// The output filename for the snapshot. This will be created in the outDir.
	SnapshotFile string

	// Whether to prefetch test packages. This is only useful when fetching
	// packages ephemerally.
	PrefetchPackages bool

	// Whether to use serial to run tests on the target.
	UseSerial bool

	// The ffx instance to use.
	FFX *ffxutil.FFXInstance

	// The level of experimental ffx features to enable.
	//
	// See //tools/botanist/cmd/run.go for the mapping of features to levels.
	FFXExperimentLevel int

	// Whether to upload to upload test results to ResultDB from testrunner
	// Bool enables soft transition from tefmocheck running in the recipe vs the processing happening in botanist.
	UploadToResultDB bool

	// The path to the llvm-profdata binary to use to merge profiles on the host.
	// If given as `<path>=<version>`, the version should correspond to the version
	// of the profiles produced by the tests that are run.")
	LLVMProfdataPath string
}

// ScaleTestTimeout multiplies the timeout by a factor set by the TEST_TIMEOUT_SCALE_FACTOR
// environment variable. This allows us to scale timeouts on environments where
// processes run much slower than others.
func ScaleTestTimeout(timeout time.Duration) time.Duration {
	if n, err := strconv.Atoi(os.Getenv(constants.TestTimeoutScaleFactor)); err == nil {
		return timeout * time.Duration(n)
	}
	return timeout
}

func SetupAndExecute(ctx context.Context, opts Options, testsPath string) error {
	// Our mDNS library doesn't use the logger library.
	const logFlags = log.Ltime | log.Lmicroseconds | log.Lshortfile
	log.SetFlags(logFlags)

	tests, err := loadTests(testsPath)
	if err != nil {
		return fmt.Errorf("failed to load tests from %q: %w", testsPath, err)
	}

	// Configure a test outputs object, responsible for producing TAP output,
	// recording data sinks, and archiving other test outputs.
	testOutDir := filepath.Join(os.Getenv(constants.TestOutDirEnvKey), opts.OutDir)
	if testOutDir == "" {
		var err error
		testOutDir, err = os.MkdirTemp("", "testrunner")
		if err != nil {
			return fmt.Errorf("failed to create a test output directory")
		}
	}
	logger.Debugf(ctx, "test output directory: %s", testOutDir)

	var addr net.IPAddr
	if deviceAddr, ok := os.LookupEnv(botanistconstants.DeviceAddrEnvKey); ok {
		addrPtr, err := net.ResolveIPAddr("ip", deviceAddr)
		if err != nil {
			return fmt.Errorf("failed to parse device address %s: %w", deviceAddr, err)
		}
		addr = *addrPtr
	}
	sshKeyFile := os.Getenv(botanistconstants.SSHKeyEnvKey)
	serialSocketPath := os.Getenv(botanistconstants.SerialSocketEnvKey)
	// If the TestOutDirEnvKey was set, that means testrunner is being run
	// in an infra setting and thus needs an isolated environment.
	_, needsIsolatedEnv := os.LookupEnv(constants.TestOutDirEnvKey)
	// However, if testrunner is called from botanist, it doesn't need to set
	// up its own isolated environment because botanist will already have
	// done that. Botanist will set either the sshKeyFile or serialSocketPath,
	// so if neither are set, then testrunner was NOT called from botanist
	// and thus needs its own isolated environment.
	if needsIsolatedEnv {
		needsIsolatedEnv = sshKeyFile == "" && serialSocketPath == ""
	}
	cleanUp, err := environment.Ensure(needsIsolatedEnv)
	if err != nil {
		return fmt.Errorf("failed to setup environment: %w", err)
	}
	defer cleanUp()

	tapProducer := tap.NewProducer(os.Stdout)
	tapProducer.Plan(len(tests))
	outputs, err := CreateTestOutputs(tapProducer, testOutDir)
	if err != nil {
		return fmt.Errorf("failed to create test outputs: %w", err)
	}

	execErr := execute(ctx, tests, outputs, addr, sshKeyFile, serialSocketPath, testOutDir, opts)
	if err := outputs.Close(); err != nil {
		if execErr == nil {
			return err
		}
		logger.Warningf(ctx, "Failed to save test outputs: %s", err)
	}
	return execErr
}

// for testability
var (
	sshTester    = NewFuchsiaSSHTester
	serialTester = NewFuchsiaSerialTester
)

// ffxInstance takes a *ffxutil.FFXInstance and returns it as a FFXInstance
// interface. This is used for testability so that we can return a mock
// instance instead.
var ffxInstance = func(
	ctx context.Context,
	ffxInstance *ffxutil.FFXInstance,
	ffxExperimentalLevel int,
) (FFXInstance, error) {
	ffx, err := func() (FFXInstance, error) {
		if ffxInstance == nil {
			// Return nil instead of ffx so that the returned FFXInstance
			// will be the nil interface instead of an interface holding
			// a nil value. In the latter case, checking ffx == nil will
			// return false.
			return nil, nil
		}
		if ffxExperimentalLevel == 3 {
			if err := ffxInstance.ConfigSet(ctx, "test.enable_experimental_parallel_execution", "true"); err != nil {
				return ffxInstance, err
			}
		}
		// Print the config for debugging purposes.
		// TODO(ihuh): Remove when not needed.
		if err := ffxInstance.GetConfig(ctx); err != nil {
			return ffxInstance, err
		}
		return ffxInstance, nil
	}()
	return ffx, err
}

func execute(
	ctx context.Context,
	tests []testsharder.Test,
	outputs *TestOutputs,
	addr net.IPAddr,
	sshKeyFile,
	serialSocketPath,
	outDir string,
	opts Options,
) error {
	var fuchsiaSinks, localSinks []runtests.DataSinkReference
	var fuchsiaTester, localTester Tester

	localEnv := append(os.Environ(),
		// Tell tests written in Rust to print stack on failures.
		"RUST_BACKTRACE=1",
	)

	if !opts.UseSerial && sshKeyFile != "" {
		ffx, err := ffxInstance(ctx, opts.FFX, opts.FFXExperimentLevel)
		if err != nil {
			return err
		}
		if ffx != nil {
			t, err := sshTester(
				ctx, addr, sshKeyFile, outputs.OutDir, serialSocketPath)
			if err != nil {
				return fmt.Errorf("failed to initialize fuchsia tester: %w", err)
			}
			ffxTester, err := NewFFXTester(ctx, ffx, t, outputs.OutDir, opts.FFXExperimentLevel, opts.LLVMProfdataPath)
			if err != nil {
				return fmt.Errorf("failed to initialize ffx tester: %w", err)
			}
			defer func() {
				// TODO(https://fxbug.dev/42075455): Once profiles are being merged on the host, this
				// will leave empty artifact directories within each test's output directories
				// from where the profiles originated from. Clean up empty directories.
				if err := ffxTester.RemoveAllEmptyOutputDirs(); err != nil {
					logger.Debugf(ctx, "%s", err)
				}
			}()
			// Prefetching packages may possibly interfere with test execution and cause tests
			// to time out or fail, so disable when using `ffx test`.
			if ffxTester.EnabledForTesting() {
				opts.PrefetchPackages = false
			}
			fuchsiaTester = ffxTester
		}

		if opts.PrefetchPackages {
			// TODO(rudymathu): Remove this prefetching of packages once package
			// delivery is fast enough.
			resolveCtx, cancel := context.WithCancel(ctx)
			var wg sync.WaitGroup
			wg.Add(1)
			go func() {
				defer wg.Done()
				resolveLog := filepath.Join(outDir, "resolve.log")
				if err := ResolveTestPackages(resolveCtx, tests, addr, sshKeyFile, resolveLog); err != nil {
					logger.Warningf(ctx, "package pre-fetching routine failed: %s", err)
				}
			}()
			// We wait here to ensure that our log of resolved packages is
			// correctly saved.
			defer wg.Wait()
			defer cancel()
		}

	}

	// Function to select the tester to use for a test, along with destination
	// for the test to write any data sinks. This logic is not easily testable
	// because it requires a lot of network requests and environment inspection,
	// so we use dependency injection and pass it as a parameter to
	// `runAndOutputTests` to make that function more easily testable.
	testerForTest := func(test testsharder.Test) (Tester, *[]runtests.DataSinkReference, error) {
		switch test.OS {
		case "fuchsia":
			if fuchsiaTester == nil {
				var err error
				if !opts.UseSerial && sshKeyFile != "" {
					fuchsiaTester, err = sshTester(
						ctx, addr, sshKeyFile, outputs.OutDir, serialSocketPath)
				} else {
					if serialSocketPath == "" {
						return nil, nil, fmt.Errorf("%q must be set if %q is not set", botanistconstants.SerialSocketEnvKey, botanistconstants.SSHKeyEnvKey)
					}
					fuchsiaTester, err = serialTester(ctx, serialSocketPath)
				}
				if err != nil {
					return nil, nil, fmt.Errorf("failed to initialize fuchsia tester: %w", err)
				}
			}
			return fuchsiaTester, &fuchsiaSinks, nil
		case "linux", "mac":
			if test.OS == "linux" && runtime.GOOS != "linux" {
				return nil, nil, fmt.Errorf("cannot run linux tests when GOOS = %q", runtime.GOOS)
			}
			if test.OS == "mac" && runtime.GOOS != "darwin" {
				return nil, nil, fmt.Errorf("cannot run mac tests when GOOS = %q", runtime.GOOS)
			}
			// Initialize the fuchsia SSH tester to run the snapshot at the end in case
			// we ran any host-target interaction tests.
			if !opts.UseSerial && fuchsiaTester == nil && sshKeyFile != "" {
				var err error
				fuchsiaTester, err = sshTester(
					ctx, addr, sshKeyFile, outputs.OutDir, serialSocketPath)
				if err != nil {
					logger.Errorf(ctx, "failed to initialize fuchsia tester: %s", err)
				}
			}
			if localTester == nil {
				var err error
				localTester, err = NewSubprocessTester(opts.LocalWD, localEnv, outputs.OutDir, opts.NsjailPath, opts.NsjailRoot)
				if err != nil {
					return nil, nil, err
				}
			}
			return localTester, &localSinks, nil
		default:
			return nil, nil, fmt.Errorf("test %#v has unsupported OS: %q", test, test.OS)
		}
	}

	var client *resultdb.Client
	var err error
	if opts.UploadToResultDB {
		client, err = resultdb.NewClient()
		if err != nil {
			return err
		}
	}
	var finalError error
	if err := runAndOutputTests(ctx, tests, testerForTest, outputs, outDir, client); err != nil {
		finalError = err
	}

	if fuchsiaTester != nil {
		defer fuchsiaTester.Close()
	}
	if localTester != nil {
		defer localTester.Close()
	}
	finalize := func(t Tester, sinks []runtests.DataSinkReference) error {
		if t != nil {
			snapshotCtx := ctx
			if ctx.Err() != nil {
				// Run snapshot with a new context so we can still capture a snapshot even
				// if we hit a timeout. The timeout for running the snapshot should be long
				// enough to complete the command and short enough to fit within the
				// cleanupGracePeriod in //tools/lib/subprocess/subprocess.go.
				var cancel context.CancelFunc
				snapshotCtx, cancel = context.WithTimeout(context.Background(), 7*time.Second)
				defer cancel()
			}
			if err := t.RunSnapshot(snapshotCtx, opts.SnapshotFile); err != nil {
				// This error usually has a different root cause that gets masked when we
				// return this error. Log it so we can keep track of it, but don't fail.
				logger.Errorf(snapshotCtx, err.Error())
			}
			if ctx.Err() != nil {
				// If the original context was cancelled, just return the context error.
				return ctx.Err()
			}
			if err := t.EnsureSinks(ctx, sinks, outputs); err != nil {
				return err
			}
		}
		return nil
	}

	if err := finalize(localTester, localSinks); err != nil && finalError == nil {
		finalError = err
	}
	if err := finalize(fuchsiaTester, fuchsiaSinks); err != nil && finalError == nil {
		finalError = err
	}
	return finalError
}

func validateTest(test testsharder.Test) error {
	if test.Name == "" {
		return fmt.Errorf("one or more tests missing `name` field")
	}
	if test.OS == "" {
		return fmt.Errorf("one or more tests missing `os` field")
	}
	if test.Runs <= 0 {
		return fmt.Errorf("one or more tests with invalid `runs` field")
	}
	if test.Runs > 1 {
		switch test.RunAlgorithm {
		case testsharder.KeepGoing, testsharder.StopOnFailure, testsharder.StopOnSuccess:
		default:
			return fmt.Errorf("one or more tests with invalid `run_algorithm` field")
		}
	}
	if test.OS == "fuchsia" && test.PackageURL == "" && test.Path == "" {
		return fmt.Errorf("one or more fuchsia tests missing the `path` and `package_url` fields")
	}
	if test.OS != "fuchsia" {
		if test.PackageURL != "" {
			return fmt.Errorf("one or more host tests have a `package_url` field present")
		} else if test.Path == "" {
			return fmt.Errorf("one or more host tests missing the `path` field")
		}
	}
	return nil
}

func loadTests(path string) ([]testsharder.Test, error) {
	bytes, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read %q: %w", path, err)
	}

	var tests []testsharder.Test
	if err := json.Unmarshal(bytes, &tests); err != nil {
		return nil, fmt.Errorf("failed to unmarshal %q: %w", path, err)
	}

	for _, test := range tests {
		if err := validateTest(test); err != nil {
			return nil, err
		}
	}

	return tests, nil
}

// testToRun represents an entry in a queue of tests to run.
type testToRun struct {
	testsharder.Test
	// The number of times the test has already been run.
	previousRuns int
	// The sum of the durations of all the test's previous runs.
	totalDuration time.Duration
}

// runAndOutputTests runs all the tests, possibly with retries, and records the
// results to `outputs`.
// TODO(danikay): Make this testable by extracting an interface for resultdb
func runAndOutputTests(
	ctx context.Context,
	tests []testsharder.Test,
	testerForTest func(testsharder.Test) (Tester, *[]runtests.DataSinkReference, error),
	outputs *TestOutputs,
	globalOutDir string,
	resultdbClient *resultdb.Client,
) error {
	// Since only a single goroutine writes to and reads from the queue it would
	// be more appropriate to use a true Queue data structure, but we'd need to
	// implement that ourselves so it's easier to just use a channel. Make the
	// channel double the necessary size just to be safe and avoid potential
	// deadlocks.
	testQueue := make(chan testToRun, 2*len(tests))

	for _, test := range tests {
		testQueue <- testToRun{Test: test}
	}

	var resultdbResults []*sinkpb.TestResult

	// `for test := range testQueue` might seem simpler, but it would block
	// instead of exiting once the queue becomes empty. To exit the loop we
	// would need to close the channel when it became empty. That would require
	// a length check within the loop body anyway, and it's more robust to put
	// the length check in the for loop condition.
	testIndex := 0
	for len(testQueue) > 0 {
		test := <-testQueue

		t, sinks, err := testerForTest(test.Test)
		if err != nil {
			return err
		}

		var result *TestResult
		var outDir string
		if err := retryOnConnectionFailure(ctx, t, func() error {
			runIndex := test.previousRuns

			outDir = filepath.Join(globalOutDir, url.PathEscape(strings.ReplaceAll(test.Name, ":", "")), strconv.Itoa(runIndex))
			var testErr error
			result, testErr = runTestOnce(ctx, test.Test, t, outDir, testIndex)
			if result == nil {
				return testErr
			}
			result.RunIndex = runIndex

			// TODO(ihuh): Increase the runs so that we keep the outputs of all runs
			// in the task outputs, but temporarily do not record the test failure if
			// it's a connection failure so that we can correlate the connection failures
			// better through a tefmocheck.
			test.previousRuns++
			test.totalDuration += result.Duration()
			return testErr
		}); err != nil {
			return err
		}

		if err := outputs.Record(ctx, *result); err != nil {
			return err
		}
		testIndex++

		// TODO(danikay): Temporarily using the existence of the resultdb client to
		// enable the soft transition from uploading to resultdb from the fuchsia.py
		// recipe to uploading the test results here, as they complete
		if resultdbClient != nil {
			testTags := testTagsToStringPairs(result.Tags)
			testDetails := testDetailsFromTestResult(result.Name, result.StartTime, result)
			testResults, _ := resultdb.TestCaseToResultSink(result.Cases, testTags, &testDetails, outDir)
			resultdbResults = append(resultdbResults, testResults...)
		}

		if shouldKeepGoing(test.Test, result, test.totalDuration) {
			// Schedule the test to be run again.
			testQueue <- test
		}
		// TODO(olivernewman): Add a unit test to make sure data sinks are
		// recorded correctly.
		*sinks = append(*sinks, result.DataSinks)
	}
	// TODO @danikay upload test results immediately as they happen, in a background
	// go routine
	if resultdbClient != nil {
		if err := resultdbClient.ReportTestResults(resultdb.CreateTestResultsRequests(resultdbResults, 500)); err != nil {
			return err
		}
	}
	return nil
}

type connectionError struct {
	error
}

func isConnectionError(err error) bool {
	var connErr connectionError
	return errors.As(err, &connErr)
}

var connectionErrorRetryBackoff retry.Backoff = retry.NewConstantBackoff(time.Second)

func retryOnConnectionFailure(ctx context.Context, t Tester, execFunc func() error) error {
	return retry.Retry(ctx, retry.WithMaxAttempts(connectionErrorRetryBackoff, maxReconnectAttempts), func() error {
		err := execFunc()
		if isConnectionError(err) {
			logger.Errorf(ctx, "attempting to reconnect after error: %s", err)
			if reconnectErr := t.Reconnect(ctx); reconnectErr != nil {
				logger.Errorf(ctx, "%s", reconnectErr)
				// If we fail to reconnect, continuing is likely hopeless.
				// Return the *original* error (which will generally be more
				// closely related to the root cause of the failure) rather than
				// the reconnection error.
				return retry.Fatal(err)
			}
			return err
		}
		return retry.Fatal(err)
	}, nil)
}

// shouldKeepGoing returns whether we should schedule another run of the test.
// It'll return true if we haven't yet exceeded the time limit for reruns, or
// if the most recent test run didn't meet the stop condition for this test.
func shouldKeepGoing(test testsharder.Test, lastResult *TestResult, testTotalDuration time.Duration) bool {
	stopRepeatingDuration := time.Duration(test.StopRepeatingAfterSecs) * time.Second
	if stopRepeatingDuration > 0 && testTotalDuration >= stopRepeatingDuration {
		return false
	} else if test.Runs > 0 && lastResult.RunIndex+1 >= test.Runs {
		return false
	} else if test.RunAlgorithm == testsharder.StopOnSuccess && lastResult.Passed() {
		return false
	} else if test.RunAlgorithm == testsharder.StopOnFailure && !lastResult.Passed() {
		return false
	}
	return true
}

// stdioBuffer is a simple thread-safe wrapper around bytes.Buffer. It
// implements the io.Writer interface.
type stdioBuffer struct {
	// Used to protect access to `buf`.
	mu sync.Mutex

	// The underlying buffer.
	buf bytes.Buffer
}

func (b *stdioBuffer) Write(p []byte) (n int, err error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

// runTestOnce runs the given test once. It will not return an error if the test
// fails, only if an unrecoverable error occurs or testing should otherwise stop.
func runTestOnce(
	ctx context.Context,
	origTest testsharder.Test,
	t Tester,
	outDir string,
	testIndex int,
) (*TestResult, error) {
	// The test case parser specifically uses stdout, so we need to have a
	// dedicated stdout buffer.
	stdoutForParsing := new(bytes.Buffer)
	stdio := new(stdioBuffer)

	stdout, stderr, flush := botanist.NewStdioWriters(ctx, fmt.Sprintf("test%d", testIndex))
	defer flush()
	multistdout := io.MultiWriter(stdout, stdio, stdoutForParsing)
	multistderr := io.MultiWriter(stderr, stdio)

	// In the case of running tests on QEMU over serial, we do not wish to
	// forward test output to stdout, as QEMU is already redirecting serial
	// output there: we do not want to double-print.
	//
	// This is a bit of a hack, but is a lesser evil than extending the
	// testrunner CLI just to sidecar the information of 'is QEMU'.
	againstQEMU := os.Getenv(botanistconstants.NodenameEnvKey) == targets.DefaultEmulatorNodename
	if _, ok := t.(*FuchsiaSerialTester); ok && againstQEMU {
		multistdout = io.MultiWriter(stdio, stdoutForParsing)
	}

	test := origTest
	test.Timeout = ScaleTestTimeout(test.Timeout)
	logger.Debugf(ctx, "test timeout: %v, orig timeout: %v\n", test.Timeout, origTest.Timeout)

	startTime := clock.Now(ctx)

	// Set the outer timeout to a slightly higher value in order to give the tester
	// time to handle the timeout itself.  Other steps such as retrying tests over
	// serial or fetching data sink references may also cause the Test() method to
	// exceed the test's timeout, so we give enough time for the tester to
	// complete those steps as well.
	outerTestTimeout := test.Timeout + testTimeoutGracePeriod

	var timeoutCh <-chan time.Time
	if test.Timeout > 0 {
		// Intentionally call After(), thereby resolving a completion deadline,
		// *before* starting to run the test. This helps avoid race conditions
		// in this function's unit tests that advance the fake clock's time
		// within the `t.Test()` call.
		timeoutCh = clock.After(ctx, outerTestTimeout)
	}
	// Else, timeoutCh will be nil. Receiving from a nil channel blocks forever,
	// so no timeout will be enforced, which is what we want.

	type testResult struct {
		result *TestResult
		err    error
	}
	ch := make(chan testResult, 1)

	// We don't use context.WithTimeout() because it uses the real time.Now()
	// instead of clock.Now(), which makes it much harder to simulate timeouts
	// in this function's unit tests.
	testCtx, cancelTest := context.WithCancel(ctx)
	defer cancelTest()
	// Run the test in a goroutine so that we don't block in case the tester fails
	// to respect the timeout.
	go func() {
		result, err := t.Test(testCtx, test, multistdout, multistderr, outDir)
		ch <- testResult{result, err}
	}()

	result := BaseTestResultFromTest(test)
	// In the case of a timeout, store whether it hit the inner or outer test
	// timeout.
	var timeout time.Duration
	var err error
	select {
	case res := <-ch:
		result, err = t.ProcessResult(testCtx, test, outDir, res.result, res.err)
		timeout = test.Timeout
	case <-timeoutCh:
		result.Result = runtests.TestAborted
		timeout = outerTestTimeout
		cancelTest()
	}

	if err != nil && !isConnectionError(err) {
		// The tester encountered a fatal condition and cannot run any more
		// tests.
		return nil, err
	}
	if !result.Passed() && ctx.Err() != nil {
		// testrunner is shutting down, give up running tests and don't
		// report this test result as it may have been impacted by the
		// context cancelation.
		return nil, ctx.Err()
	}

	switch result.Result {
	case runtests.TestFailure:
		logger.Errorf(ctx, "Test %s failed: %s", test.Name, result.FailReason)
	case runtests.TestAborted:
		logger.Errorf(ctx, "Test %s timed out after %s", test.Name, timeout)
	}

	endTime := clock.Now(ctx)

	// Record the test details in the summary.
	result.Stdio = stdio.buf.Bytes()
	// Only the FFXTester handles cases and output files on its own. Otherwise,
	// parse the stdout for test cases and check the outdir for output files.
	if len(result.Cases) == 0 && len(result.OutputFiles) == 0 {
		result.Cases = testparser.Parse(stdoutForParsing.Bytes())
		caseOutputFiles := []string{}
		for _, tc := range result.Cases {
			for _, of := range tc.OutputFiles {
				caseOutputFiles = append(caseOutputFiles, filepath.Join(tc.OutputDir, of))
			}
		}

		if err := filepath.WalkDir(outDir, func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if d.IsDir() {
				return nil
			}
			relPath, err := filepath.Rel(outDir, path)
			if err != nil {
				return err
			}
			// Don't include the file if it's already recorded as a test case output file.
			if !strings.Contains(strings.Join(caseOutputFiles, " "), path) {
				result.OutputFiles = append(result.OutputFiles, relPath)
			}
			return nil
		}); err != nil && !os.IsNotExist(err) {
			logger.Debugf(ctx, "unable to record output files: %s", err)
		}
		if len(result.OutputFiles) > 0 {
			result.OutputDir = outDir
		}
	} else {
		// TODO(b/311443213): Some tests rely on testparser to add tags to test case results.
		// Remove this hack once tags are properly handled by `ffx test`.
		cases := testparser.Parse(stdoutForParsing.Bytes())
		caseToTags := make(map[string][]build.TestTag)
		for _, tc := range cases {
			caseToTags[tc.DisplayName] = tc.Tags
		}
		for i, tc := range result.Cases {
			result.Cases[i].Tags = append(result.Cases[i].Tags, caseToTags[tc.DisplayName]...)
		}
	}

	// The start time and end time should cover the entire duration to run the test
	// and process the results.
	result.StartTime = startTime
	result.EndTime = endTime
	result.Affected = test.Affected
	return result, err
}

// Helper function to convert []build.TestTag to []resultpb.StringPair
func testTagsToStringPairs(tags []build.TestTag) []*resultpb.StringPair {
	stringPairs := make([]*resultpb.StringPair, 0, len(tags))

	for _, tag := range tags {
		stringPairs = append(stringPairs, &resultpb.StringPair{
			Key:   tag.Key,
			Value: tag.Value,
		})
	}

	return stringPairs
}

// Helper function to create runtests.TestDetails from result.TestResult
func testDetailsFromTestResult(name string, startTime time.Time, result *TestResult) runtests.TestDetails {
	return runtests.TestDetails{
		Name:      name,
		StartTime: startTime,
		Result:    result.Result,
	}
}
