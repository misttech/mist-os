// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

// Program to watch for a specific string to appear from a socket's output and
// then exits successfully.

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"net"
	"os"
	"path/filepath"
	"time"

	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/iomisc"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"
)

var (
	timeout        time.Duration
	successString  string
	redirectStdout bool
)

func init() {
	flag.DurationVar(&timeout, "timeout", 10*time.Minute, "amount of time to wait for success string")
	flag.BoolVar(&redirectStdout, "stdout", false, "whether to redirect serial output to stdout")
	flag.StringVar(&successString, "success-str", "", "string that - if read - indicates success")
}

// TODO(fxbug.dev/116559): Revisit as this is a workaround for a possibly lower-level bug.
type socketReader struct {
	ctx     context.Context
	r       net.Conn
	timeout time.Duration
}

func (s *socketReader) Read(p []byte) (int, error) {
	for {
		if err := s.r.SetReadDeadline(time.Now().Add(s.timeout)); err != nil {
			return 0, err
		}
		n, err := s.r.Read(p)
		var netErr net.Error
		if errors.As(err, &netErr) {
			// If the error was due to an IO timeout, try reading again.
			if netErr.Timeout() {
				logger.Debugf(s.ctx, "%s", netErr)
				continue
			}
		}
		return n, err
	}
}

func execute(ctx context.Context, socketPath string, stdout io.Writer) error {
	if socketPath == "" {
		flag.Usage()
		return fmt.Errorf("could not find socket in environment")
	}
	logger.Debugf(ctx, "socket: %s", socketPath)

	if successString == "" {
		flag.Usage()
		return fmt.Errorf("-success is a required argument")
	}

	socket, err := net.Dial("unix", socketPath)
	if err != nil {
		return err
	}
	defer socket.Close()

	socketTee := io.TeeReader(&socketReader{ctx, socket, 10 * time.Second}, stdout)

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	// Print out a log periodically to give an estimate of the timestamp at which
	// logs are getting read from the socket.
	tickerSecs := math.Min(30, timeout.Seconds()/2)
	ticker := time.NewTicker(time.Duration(tickerSecs) * time.Second)
	defer ticker.Stop()
	go func() {
		for range ticker.C {
			logger.Debugf(ctx, "still running test...")
		}
	}()

	if _, err := iomisc.ReadUntilMatchString(ctx, socketTee, successString); err != nil {
		if ctx.Err() != nil {
			return fmt.Errorf("timed out before success string %q was read from serial", successString)
		}
		return fmt.Errorf("error trying to read from socket: %w", err)
	}
	logger.Debugf(ctx, "success string found: %q", successString)
	return nil
}

func main() {
	flag.Parse()

	log := logger.NewLogger(logger.DebugLevel, color.NewColor(color.ColorAuto),
		os.Stdout, os.Stderr, "seriallistener ")
	ctx := logger.WithLogger(context.Background(), log)

	// Emulator serial is already wired up to stdout
	// TODO(fxbug.dev/116559): Temporarily write serial output
	// to a file for debugging purposes.
	stdout := io.Discard
	if outDir := os.Getenv(testrunnerconstants.TestOutDirEnvKey); outDir != "" {
		if serialOutput, err := osmisc.CreateFile(filepath.Join(outDir, "serial_output")); err != nil {
			logger.Errorf(ctx, "%s", err)
		} else {
			stdout = serialOutput
			// Have the logger write to the file as well to get a
			// better sense of how much is read from the socket before
			// the socket io or ticker timeouts are reached.
			log := logger.NewLogger(logger.DebugLevel, color.NewColor(color.ColorAuto),
				io.MultiWriter(os.Stdout, serialOutput), io.MultiWriter(os.Stderr, serialOutput), "seriallistener ")
			ctx = logger.WithLogger(ctx, log)
		}
	}
	deviceType := os.Getenv(constants.DeviceTypeEnvKey)
	if deviceType != "QEMU" && deviceType != "AEMU" {
		stdout = os.Stdout
	}

	socketPath := os.Getenv(constants.SerialSocketEnvKey)
	if err := execute(ctx, socketPath, stdout); err != nil {
		logger.Fatalf(ctx, "%s", err)
	}
}
