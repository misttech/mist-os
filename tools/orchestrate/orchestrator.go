// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package orchestrate

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	ffx "go.fuchsia.dev/fuchsia/tools/orchestrate/ffx"
	utils "go.fuchsia.dev/fuchsia/tools/orchestrate/utils"
)

// TestOrchestrator uses FFX to run Fuchsia component tests.
type TestOrchestrator struct {
	ffx           *ffx.Ffx
	deviceConfig  *DeviceConfig
	ffxLogProc    *os.Process
	targetLogFile *os.File
}

var (
	ffxDaemonLog  = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "ffx_daemon.log")
	ffxConfigDump = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "ffx_config.txt")
	subrunnerLog  = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "subrunner.log")
	targetLog     = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "target.log")
	targetSymLog  = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "target.symbolized.log")
	summaryPath   = filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "summary.json")
)

// NewTestOrchestrator creates a TestOrchestrator with default dependencies.
func NewTestOrchestrator(deviceConfig *DeviceConfig) *TestOrchestrator {
	return &TestOrchestrator{
		deviceConfig: deviceConfig,
	}
}

func (r *TestOrchestrator) instantiateFfx(in *RunInput) error {
	wd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("os.Getwd: %w", err)
	}
	ffxOpt := &ffx.Option{
		ExePath: filepath.Join(wd, in.Target().FfxPath),
		LogDir:  os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"),
	}
	f, err := ffx.New(ffxOpt)
	if err != nil {
		return fmt.Errorf("ffx.New: %w", err)
	}
	r.ffx = f
	return nil
}

// Run executes tests.
func (r *TestOrchestrator) Run(in *RunInput, testCmd []string) error {
	if len(in.Cipd()) > 0 {
		fmt.Println("=== orchestrate - Downloading CIPD packages (0/6) ===")
		if err := r.cipdEnsure(in); err != nil {
			return fmt.Errorf("cipdEnsure: %w", err)
		}
	}
	if in.IsTarget() {
		fmt.Println("=== orchestrate - Setting up ffx (1/6) ===")
		if err := r.instantiateFfx(in); err != nil {
			return fmt.Errorf("instantiateFfx: %w", err)
		}
		defer func() {
			if err := r.ffx.Close(); err != nil {
				fmt.Printf("ffx.Close: %v\n", err)
			}
		}()
		if err := r.setupFfx(); err != nil {
			return fmt.Errorf("setupFfx: %w", err)
		}
		defer r.stopDaemon()
		productDir := ""
		if in.Target().TransferURL != "" {
			fmt.Println("=== orchestrate - Downloading Product Bundle (2/6) ===")
			var err error
			productDir, err = r.downloadProductBundle(in)
			if err != nil {
				return fmt.Errorf("downloadProductBundle: %w", err)
			}
		} else if in.Target().LocalPB != "" {
			fmt.Println("=== orchestrate - Local Product Bundle (2/6) ===")
			productDir = in.Target().LocalPB
		}
		if in.IsHardware() {
			fmt.Println("=== orchestrate - Flashing Device (3/6) ===")
			if err := r.flashDevice(productDir); err != nil {
				return fmt.Errorf("flashDevice: %w", err)
			}
		} else if in.IsEmulator() {
			fmt.Println("=== orchestrate - Starting Emulator (3/6) ===")
			if err := r.startEmulator(productDir); err != nil {
				return fmt.Errorf("startEmulator: %w", err)
			}
			defer r.stopEmulator()
		}
		fmt.Println("=== orchestrate - Serving Packages (4/6) ===")
		if err := r.servePackages(in, productDir); err != nil {
			return fmt.Errorf("servePackages: %w", err)
		}
		defer r.stopPackageServer()
		fmt.Println("=== orchestrate - Reach Device (5/6) ===")
		if err := r.reachDevice(); err != nil {
			return fmt.Errorf("reachDevice: %w", err)
		}
		defer r.stopFfxLog()
	} else {
		fmt.Println("=== orchestrate - Skipped Target Provisioning (1-5/6) ===")
	}
	fmt.Println("=== orchestrate - Test (6/6) ===")
	if err := r.test(testCmd, in); err != nil {
		return fmt.Errorf("test: %w", err)
	}
	return nil
}

/* Step 0 - Downloading CIPD packages. */
func (r *TestOrchestrator) cipdEnsure(in *RunInput) error {
	wd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("os.Getwd: %w", err)
	}
	for destPath, cipdSpec := range in.Cipd() {
		split := strings.SplitN(cipdSpec, ":", 2)
		ensureLine := fmt.Sprintf("%s\t%s\n", split[0], split[1])
		cipdCmd := []string{
			"cipd",
			"ensure",
			"-ensure-file",
			"-",
			"-root",
			filepath.Join(wd, destPath),
			"-service-account-json",
			":gce",
		}
		fmt.Printf("Running command: %+v stdin: %s", cipdCmd, ensureLine)
		cmd := exec.Command(cipdCmd[0], cipdCmd[1:]...)
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		cmd.Stdin = strings.NewReader(ensureLine)
		cmd.Env = os.Environ()
		if err := cmd.Run(); err != nil {
			return fmt.Errorf("cmd.Run: %w", err)
		}
	}
	return nil
}

/* Step 1 - Setting up ffx. */
func (r *TestOrchestrator) setupFfx() error {
	cmds := [][]string{
		{"config", "set", "log.level", "Debug"},
		{"config", "set", "test.experimental_json_input", "true"},
		{"config", "set", "fastboot.flash.timeout_rate", "4"},
		{"config", "set", "discovery.mdns.enabled", "false"},
		{"config", "set", "fastboot.usb.disabled", "true"},
		{"config", "set", "proactive_log.enabled", "false"},
		{"config", "set", "daemon.autostart", "false"},
		{"config", "set", "overnet.cso", "only"},
		{"config", "set", "ffx-repo-add", "true"},
	}
	for _, cmd := range cmds {
		if out, err := r.ffx.RunCmdSync(cmd...); err != nil {
			return fmt.Errorf("ffx setup %v: %w out: %s", cmd, err, out)
		}
	}
	if err := r.dumpFfxConfig(); err != nil {
		return fmt.Errorf("dumpFfxConfig: %w", err)
	}
	if err := r.daemonStart(); err != nil {
		return fmt.Errorf("ffx daemon start: %w", err)
	}
	if err := r.ffx.WaitForDaemon(context.Background()); err != nil {
		return fmt.Errorf("ffx daemon wait: %w", err)
	}
	return nil
}

func (r *TestOrchestrator) dumpFfxConfig() error {
	logFile, err := os.Create(ffxConfigDump)
	if err != nil {
		return fmt.Errorf("os.Create: %w", err)
	}
	defer func() {
		if err := logFile.Close(); err != nil {
			fmt.Printf("logFile.Close: %v\n", err)
		}
	}()
	cmd, err := r.ffx.Cmd("config", "get")
	if err != nil {
		return fmt.Errorf("ffx.Cmd: %v", err)
	}
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	return cmd.Run()
}

func (r *TestOrchestrator) daemonStart() error {
	logFile, err := os.Create(ffxDaemonLog)
	if err != nil {
		return fmt.Errorf("os.Create: %w", err)
	}
	cmd, err := r.ffx.Cmd("daemon", "start")
	if err != nil {
		return fmt.Errorf("ffx.Cmd: %v", err)
	}
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	return cmd.Start()
}

/* Step 2 - Downloading product bundle. */
func (r *TestOrchestrator) downloadProductBundle(in *RunInput) (string, error) {
	wd, err := os.Getwd()
	if err != nil {
		return "", fmt.Errorf("os.Getwd: %w", err)
	}
	dir := filepath.Join(wd, "ffx-product-bundle")
	ffxArgs := []string{
		"product",
		"download",
		in.Target().TransferURL,
		dir,
	}
	if in.Target().FfxluciauthPath != "" {
		ffxArgs = append(ffxArgs, "--auth", in.Target().FfxluciauthPath)
	}
	_, err = r.ffx.RunCmdSync(ffxArgs...)
	if err != nil {
		return "", fmt.Errorf("ffx product download: %w", err)
	}
	return dir, nil
}

/* Step 3 - Flashing device OR Starting emulator. */
func (r *TestOrchestrator) flashDevice(productDir string) error {
	if err := r.ffx.Flash(r.deviceConfig.FastbootSerial, productDir, ""); err != nil {
		return fmt.Errorf("ffx flash: %w", err)
	}
	return nil
}

func (r *TestOrchestrator) startEmulator(productDir string) error {
	// Wait up to 5 minutes for the emulator to start up.
	// This helps with local test reproduction workflows where the host machine
	// does not have kvm enabled.
	if _, err := r.ffx.RunCmdSync(
		"emu",
		"start",
		productDir,
		"--net",
		"user",
		"--headless",
		"--startup-timeout",
		"300",
	); err != nil {
		return fmt.Errorf("ffx emu start: %w", err)
	}
	return nil
}

/* Step 4 - Serving packages. */
func (r *TestOrchestrator) servePackages(in *RunInput, productDir string) error {
	if out, err := r.ffx.RunCmdSync("repository", "add-from-pm", productDir); err != nil {
		return fmt.Errorf("ffx repository add-from-pm: %w out: %s", err, out)
	}
	// It is important to always publish, even if there is nothing in
	// in.Target().PackageArchives, because it will force the package metadata
	// to be refreshed (see b/309847820).
	publishArgs := []string{"repository", "publish", productDir}
	for _, far := range in.Target().PackageArchives {
		publishArgs = append(publishArgs, "--package-archive", far)
	}
	if out, err := r.ffx.RunCmdSync(publishArgs...); err != nil {
		return fmt.Errorf("ffx %v: %w out: %v", publishArgs, err, out)
	}
	for _, buildID := range in.Target().BuildIds {
		if out, err := r.ffx.RunCmdSync("debug", "symbol-index", "add", buildID); err != nil {
			return fmt.Errorf("ffx debug symbol-index add %s: %w out: %s", buildID, err, out)
		}
	}
	if err := r.serveAndWait(); err != nil {
		return fmt.Errorf("serveAndWait: %w", err)
	}
	if _, err := r.ffx.RunCmdSync("repository", "list"); err != nil {
		return fmt.Errorf("ffx repository list: %w", err)
	}
	return nil
}

func (r *TestOrchestrator) serveAndWait() error {
	port := os.Getenv("FUCHSIA_PACKAGE_SERVER_PORT")
	if port == "" {
		port = "8083"
	}
	addr := fmt.Sprintf("[::]:%s", port)
	if _, err := r.ffx.RunCmdAsync("repository", "server", "start", "--address", addr); err != nil {
		return fmt.Errorf("ffx repository server start: %w", err)
	}
	return utils.RunWithRetries(context.Background(), 500*time.Millisecond, 5, func() error {
		req, err := http.NewRequest("GET", fmt.Sprintf("http://localhost:%s", port), nil)
		if err != nil {
			return fmt.Errorf("http.NewRequest: %w", err)
		}
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			return fmt.Errorf("http.DefaultClient.Do: %w", err)
		}
		// Check the response status code
		if resp.StatusCode != 200 {
			return fmt.Errorf("resp.StatusCode: got %d, want 200", resp.StatusCode)
		}
		return nil
	})
}

/* Step 5 - Reach Device */
func (r *TestOrchestrator) reachDevice() error {
	if r.deviceConfig != nil {
		addr := r.deviceConfig.Network.IPv4
		if _, err := r.ffx.RunCmdSync("target", "add", addr, "--nowait"); err != nil {
			return fmt.Errorf("ffx target add: %w", err)
		}
	}
	if _, err := r.ffx.RunCmdSync("target", "wait"); err != nil {
		return fmt.Errorf("ffx target wait: %w", err)
	}
	if _, err := r.ffx.RunCmdSync("target", "show"); err != nil {
		return fmt.Errorf("ffx target show: %w", err)
	}
	if err := r.dumpFfxLog(); err != nil {
		return fmt.Errorf("dumpFfxLog: %w", err)
	}
	if out, err := r.ffx.RunCmdSync(
		"target",
		"repository",
		"register",
		"--repository",
		"devhost",
		"--alias",
		"fuchsia.com",
		"--alias",
		"chromium.org"); err != nil {
		return fmt.Errorf("ffx target repository register: %w out: %s", err, out)
	}
	return nil
}

func (r *TestOrchestrator) dumpFfxLog() error {
	logFile, err := os.Create(targetLog)
	if err != nil {
		return fmt.Errorf("os.Create: %w", err)
	}
	r.targetLogFile = logFile
	cmd, err := r.ffx.Cmd("log", "--symbolize", "off")
	if err != nil {
		return fmt.Errorf("ffx.Cmd: %v", err)
	}
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("cmd.Start: %w", err)
	}
	go func() {
		if err := cmd.Wait(); err != nil {
			fmt.Printf("cmd.Wait: %v", err)
		}
	}()
	r.ffxLogProc = cmd.Process
	return nil
}

/* Step 6 - Test */
func (r *TestOrchestrator) test(testCmd []string, in *RunInput) error {
	wd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("os.Getwd: %w", err)
	}
	logFile, err := os.Create(subrunnerLog)
	if err != nil {
		return fmt.Errorf("os.Create: %w", err)
	}
	defer func() {
		if err := logFile.Close(); err != nil {
			fmt.Printf("logFile.Close: %v\n", err)
		}
	}()

	// Prepare the env for target tests:
	//  1. Applies default ffx cmd environment variables
	//     (eg: isolation, disabling analytics).
	//  2. Adds ffx so that downstream can call "ffx" without having to leak its
	//     full path.
	//  3. Add openssh to PATH.
	env := os.Environ()
	if in.IsTarget() {
		env, err = r.ffx.ApplyEnv(env)
		if err != nil {
			return fmt.Errorf("ffx.ApplyEnv: %v", err)
		}
		ffxDir := filepath.Dir(filepath.Join(wd, in.Target().FfxPath))
		if err = os.Setenv("PATH", fmt.Sprintf("%s:%s", ffxDir, os.Getenv("PATH"))); err != nil {
			return fmt.Errorf("os.Setenv: %w", err)
		}
		env = utils.AppendPath(env, ffxDir)
	}

	// Create cmd AFTER setting the PATH so that it will correctly resolve testCmd[0]
	cmd := exec.Command(testCmd[0], testCmd[1:]...)
	cmd.Env = env

	// Setup pipes to forward subcmd stdout and stderr to logFile and os.Stdout.
	pipeOut := io.MultiWriter(logFile, os.Stdout)
	cmd.Stdout = pipeOut
	cmd.Stderr = pipeOut

	fmt.Printf("Running test: %+v\n", cmd.Args)
	testErr := cmd.Run()
	fmt.Printf("Pausing 10 seconds for log flush...\n")
	time.Sleep(10 * time.Second)
	if in.IsTarget() {
		if _, err := r.ffx.RunCmdSync("target", "snapshot", "-d", os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR")); err != nil {
			fmt.Printf("target snapshot: %v\n", err)
		}
	}
	if err := r.writeTestSummary(testErr); err != nil {
		return fmt.Errorf("writeTestSummary: %w", err)
	}
	// TODO(b/322928092): Disable and remove this once `orchestrate` is the
	// entrypoint for all bazel_build_test_upload invocations.
	if in.HasExperiment("orchestrate-error-on-test-failure") && testErr != nil {
		return fmt.Errorf("Test Failures: %w", err)
	}
	return nil
}

// testSummary determines the data for out/summary.json
type testSummary struct {
	Success bool `json:"success"`
}

func (r *TestOrchestrator) writeTestSummary(testErr error) error {
	if testErr != nil {
		fmt.Printf("Tests failed: %v\n", testErr)
	}
	summary := &testSummary{
		Success: testErr == nil,
	}
	if err := os.MkdirAll(filepath.Dir(summaryPath), 0755); err != nil {
		return fmt.Errorf("os.MkdirAll: %w", err)
	}
	if err := writeJSON(summaryPath, summary); err != nil {
		return fmt.Errorf("writeJSON: %w", err)
	}
	return nil
}

func writeJSON(filename string, data any) error {
	rawData, err := json.MarshalIndent(data, "", "  ")
	if err != nil {
		return fmt.Errorf("json.MarshalIndent: %w", err)
	}
	if err = os.WriteFile(filename, rawData, 0644); err != nil {
		return fmt.Errorf("os.WriteFile: %w", err)
	}
	return nil
}

/* Cleanup */
func (r *TestOrchestrator) stopPackageServer() {
	if _, err := r.ffx.RunCmdSync("repository", "server", "stop"); err != nil {
		fmt.Printf("ffx repository server stop: %v", err)
	}
}

func (r *TestOrchestrator) stopEmulator() {
	if _, err := r.ffx.RunCmdSync("emu", "stop", "--all"); err != nil {
		fmt.Printf("ffx emu stop: %v", err)
	}
}

func (r *TestOrchestrator) stopDaemon() {
	if _, err := r.ffx.RunCmdSync("daemon", "stop", "--no-wait"); err != nil {
		fmt.Printf("ffx daemon stop: %v", err)
	}
}

func (r *TestOrchestrator) stopFfxLog() {
	if r.ffxLogProc == nil {
		return
	}
	if err := r.ffxLogProc.Kill(); err != nil {
		fmt.Printf("ffxLogProc.Kill: %v\n", err)
	}
	if err := r.targetLogFile.Close(); err != nil {
		fmt.Printf("targetLogFile.Close: %v\n", err)
	}
	// Symbolize logs
	if err := r.Symbolize(targetLog, targetSymLog); err != nil {
		fmt.Printf("Symbolize: %v\n", err)
	}
}

// Symbolize uses ffx to symbolize the log output.
func (r *TestOrchestrator) Symbolize(input, output string) error {
	logFile, err := os.Open(input)
	if err != nil {
		return fmt.Errorf("os.Open(%q): %w", input, err)
	}
	defer func() {
		if err := logFile.Close(); err != nil {
			fmt.Printf("logFile.Close: %v\n", err)
		}
	}()
	symbolizedFile, err := os.Create(output)
	if err != nil {
		return fmt.Errorf("os.Create(%q): %w", output, err)
	}
	defer func() {
		if err := symbolizedFile.Close(); err != nil {
			fmt.Printf("symbolizedFile.Close: %v\n", err)
		}
	}()
	cmd, err := r.ffx.Cmd("debug", "symbolize")
	if err != nil {
		return fmt.Errorf("ffx.Cmd: %v", err)
	}
	cmd.Stdin = logFile
	cmd.Stdout = symbolizedFile
	cmd.Stderr = symbolizedFile
	return cmd.Run()
}
