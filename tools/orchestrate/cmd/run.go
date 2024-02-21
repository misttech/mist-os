// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"github.com/google/subcommands"
	"go.fuchsia.dev/fuchsia/tools/orchestrate"
)

type runCmd struct {
	input        string
	deviceConfig string
}

func (*runCmd) Name() string {
	return "run"
}

func (*runCmd) Synopsis() string {
	return "Runs fuchsia test in out-of-tree environment"
}

func (*runCmd) Usage() string {
	return "Usage: ./orchestrate run [flags...]"
}

func (r *runCmd) SetFlags(f *flag.FlagSet) {
	f.StringVar(&r.deviceConfig, "device-config", "/etc/botanist/config.json", "File path for device config JSON file.")
	f.StringVar(&r.input, "input", "", "File path for input JSON file.")
}

func (r *runCmd) Execute(_ context.Context, f *flag.FlagSet, _ ...any) subcommands.ExitStatus {
	if err := initTestArtifactsDir(); err != nil {
		fmt.Printf("Failed to initialize test artifacts directory: %v\n", err)
		return subcommands.ExitFailure
	}
	oc := orchestrate.NewOrchestrateConfig()
	runInput, err := oc.ReadRunInput(r.input)
	fmt.Printf("Run input: %+v\n", runInput)
	if err != nil {
		fmt.Printf("Reading run input failed: %v\n", err)
		return subcommands.ExitFailure
	}
	// Botanist's config.json and serial logging will only be present when
	// testing with hardware.
	var deviceConfig *orchestrate.DeviceConfig
	var serialLog *orchestrate.SerialLogging
	if runInput.IsHardware() {
		deviceConfig, err = oc.ReadDeviceConfig(r.deviceConfig)
		if err != nil {
			fmt.Printf("Failed to read Device Config: %v\n", err)
			return subcommands.ExitFailure
		}
		serialLog, err := orchestrate.StartSerialLogging(deviceConfig)
		if err != nil {
			fmt.Printf("Starting serial logging failed: %v\n", err)
			return subcommands.ExitFailure
		}
		defer serialLog.Stop()
	}
	if err = dumpEnv(); err != nil {
		fmt.Printf("Dumping env failed: %v\n", err)
	}
	runner := orchestrate.NewTestOrchestrator(deviceConfig)
	if serialLog != nil {
		defer serialLog.Symbolize(runner)
	}
	if err := runner.Run(runInput, f.Args()); err != nil {
		fmt.Printf("Runner failed: %v\n", err)
		return subcommands.ExitFailure
	}
	return subcommands.ExitSuccess
}

func initTestArtifactsDir() error {
	if os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR") == "" {
		fmt.Printf("Environment variable TEST_UNDECLARED_OUTPUTS_DIR is not set, test artifacts will be written to the current working directory.\n")
		return nil
	}
	if err := os.MkdirAll(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), 0755); err != nil {
		return fmt.Errorf("os.Mkdir: %w", err)
	}
	return nil
}

func dumpEnv() error {
	logFile, err := os.Create(filepath.Join(os.Getenv("TEST_UNDECLARED_OUTPUTS_DIR"), "env.dump.txt"))
	if err != nil {
		return fmt.Errorf("os.Create: %w", err)
	}
	for _, e := range os.Environ() {
		fmt.Fprintf(logFile, "%v\n", e)
	}
	if err = logFile.Close(); err != nil {
		return fmt.Errorf("logFile.Close: %w", err)
	}
	return nil
}
