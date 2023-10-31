// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
// Package measurepower contains utility functions to invoke power measurements in infra.
package idletest

import (
	"errors"
	"fmt"
	"io"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"go.fuchsia.dev/fuchsia/src/lib/go-benchmarking"
)

const testSuite = "fuchsia.vim3_monsoon_idle"

type PowerMeasurement struct {
	done     chan bool
	cmd      *exec.Cmd
	csvPath  string
	jsonPath string
	logger   *log.Logger
}

func NewPowerMeasurement(name string) (*PowerMeasurement, error) {
	logger := log.New(os.Stderr, "", log.LstdFlags)
	env := os.Environ()
	measurePowerPath, err := findMeasurePowerPath(env)
	if err != nil {
		return nil, fmt.Errorf("findMeasurePowerPath: %s", err)
	}
	wd, err := os.Getwd()
	if err != nil {
		return nil, fmt.Errorf("os.Getwd: %v", err)
	}
	csvPath := filepath.Join(wd, "..", "..", "serial_logs", fmt.Sprintf("%s.csv", name))
	jsonPath := filepath.Join(wd, "..", "..", "serial_logs", fmt.Sprintf("%s.json", name))
	cmd := exec.Command(measurePowerPath, "-format", "csv")
	cmd.Env = env
	cmd.Stderr = os.Stderr
	pm := &PowerMeasurement{done: make(chan bool), cmd: cmd, csvPath: csvPath, jsonPath: jsonPath, logger: logger}
	if err := pm.start(); err != nil {
		return nil, fmt.Errorf("start: %s", err)
	}
	return pm, nil
}

func (pm *PowerMeasurement) start() error {
	if pm.cmd.Process != nil {
		return fmt.Errorf("Already started with PID %d", pm.cmd.Process.Pid)
	}
	deferCleanup := true
	stdout, err := pm.cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("StdoutPipe: %v", err)
	}
	defer func() {
		if !deferCleanup {
			return
		}
		if err := stdout.Close(); err != nil {
			pm.logger.Printf("measurepower stdout.Close: %s", err)
		}
	}()
	outFile, err := os.Create(pm.csvPath)
	if err != nil {
		return fmt.Errorf("os.Create: %v", err)
	}
	defer func() {
		if !deferCleanup {
			return
		}
		if err := outFile.Close(); err != nil {
			pm.logger.Printf("meaaurepower outFile.Close: %s", err)
		}
	}()
	if err := pm.cmd.Start(); err != nil {
		return fmt.Errorf("cmd.Start: %v", err)
	}
	if err := copyAndBlockFirstByte(stdout, outFile); err != nil {
		return fmt.Errorf("copyAndBlockFirstByte: %v", err)
	}
	deferCleanup = false
	go func() {
		defer stdout.Close()
		defer outFile.Close()
		if err := pm.backgroundCopy(stdout, outFile); err != nil {
			pm.logger.Printf("measurepower backgroundCopy error: %s", err)
		}
		pm.done <- true
		close(pm.done)
	}()
	return nil
}

func (pm *PowerMeasurement) Stop() error {
	if pm.cmd.Process == nil {
		return errors.New("not started")
	}
	if err := pm.cmd.Process.Signal(syscall.SIGINT); err != nil {
		return fmt.Errorf("Signal SIGINT: %s", err)
	}
	if err := pm.cmd.Wait(); err != nil {
		return fmt.Errorf("Wait: %s", err)
	}
	select {
	case <-pm.done:
		return nil
	case <-time.After(5 * time.Second):
		return errors.New("Backgroundcopy goroutine did not complete and return done signal within the specified time (5sec)")
	}
	if err := outputFile(pm.jsonPath); err != nil {
		pm.logger.Printf("outputFile %q creation error: %s", pm.jsonPath, err)
	} else {
		pm.logger.Printf("Wrote benchmark values to a file: %s", pm.jsonPath)
	}
}

func (pm *PowerMeasurement) backgroundCopy(stdout io.ReadCloser, outFile *os.File) error {
	if _, err := io.Copy(outFile, stdout); err != nil {
		return fmt.Errorf("io.Copy: %s", err)
	}
	return nil
}

func copyAndBlockFirstByte(stdout io.ReadCloser, outFile *os.File) error {
	buff := make([]byte, 1)
	if _, err := stdout.Read(buff); err != nil {
		// An EOF here is unexpected and an error.
		return fmt.Errorf("stdout.Read: %v", err)
	}
	if _, err := outFile.Write(buff); err != nil {
		return fmt.Errorf("outFile.Write: %v", err)
	}
	return nil
}

func findMeasurePowerPath(env []string) (string, error) {
	for _, e := range env {
		eSplit := strings.SplitN(e, "=", 2)
		if eSplit[0] == "MEASUREPOWER_PATH" {
			return eSplit[1], nil
		}
	}
	return "", fmt.Errorf("No MEASUREPOWER_PATH on env: %s", env)
}

func outputFile(outputFilename string) error {
	results := benchmarking.TestResultsFile{
		{
			Label:     "Go/label/AveragePower",
			TestSuite: testSuite,
			Unit:      "Watt",
			Values:    []float64{100},
		},
		{
			Label:     "Go/label/AverageCurrent",
			TestSuite: testSuite,
			Unit:      "Amp",
			Values:    []float64{200},
		},
		{
			Label:     "Go/label/Duration",
			TestSuite: testSuite,
			Unit:      benchmarking.Milliseconds,
			Values:    []float64{240000},
		},
	}

	out, err := os.Create(outputFilename)
	if err != nil {
		return fmt.Errorf("failed to create file %q: %w", outputFilename, err)
	}
	defer out.Close()

	if err := results.Encode(out); err != nil {
		return fmt.Errorf("failed to write results to file %q: %w", outputFilename, err)
	}
	return nil
}
