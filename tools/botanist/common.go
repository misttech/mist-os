// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package botanist

import (
	"bytes"
	"context"
	"io"
	"math"
	"time"

	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/streams"
)

// LockedWriter is a wrapper around a writer that locks around each write so
// that multiple writes won't interleave with each other.
type LockedWriter struct {
	locks  chan *writeLock
	writer io.Writer
}

type writeLock struct {
	start chan struct{}
	end   chan struct{}
}

// NewLockedWriter returns a LockedWriter that associates a new lock with the
// provided writer.
func NewLockedWriter(ctx context.Context, writer io.Writer) *LockedWriter {
	w := &LockedWriter{
		locks:  make(chan *writeLock),
		writer: writer,
	}

	go func() {
		for lock := range w.locks {
			// Signal write to start.
			lock.start <- struct{}{}
			// Wait for write to finish.
			<-lock.end
		}
	}()
	return w
}

func (w *LockedWriter) Write(data []byte) (int, error) {
	start := make(chan struct{})
	end := make(chan struct{})
	// Queue write.
	w.locks <- &writeLock{start, end}
	// Wait for turn to start write.
	<-start
	// Defer sending struct on chan to signal end of write.
	defer func() { end <- struct{}{} }()
	return w.writer.Write(data)
}

func (w *LockedWriter) Close() {
	close(w.locks)
}

// LineWriter is a wrapper around a writer that writes line by line so
// that multiple writers to the same underlying writer won't interleave
// their writes midline.
type LineWriter struct {
	writer io.Writer
	line   []byte
}

// NewLineWriter returns a new LineWriter.
func NewLineWriter(writer io.Writer) *LineWriter {
	return &LineWriter{
		writer: writer,
	}
}

// Write stores bytes until it gets a newline and then writes to the underlying
// writer line by line. If the underlying Write() returns an err, this writer
// will return the number of bytes of the current data that were written.
// Otherwise, it returns the full length of the data to notify callers that it
// has received the whole data.
func (w *LineWriter) Write(data []byte) (int, error) {
	lines := bytes.SplitAfter(data, []byte("\n"))
	written := 0
	for _, line := range lines {
		if bytes.HasSuffix(line, []byte("\n")) {
			n, err := w.writer.Write(append(w.line, line...))
			written += int(math.Max(0, float64(n-len(w.line))))
			if err != nil {
				return written, err
			}
			w.line = []byte{}
		} else {
			w.line = append(w.line, line...)
		}
	}
	return len(data), nil
}

// TimestampWriter is a wrapper around a writer that prepends its writes
// with the current host timestamp. This will allow all botanist logs
// (kernel/serial/syslog/test) to be reliably lined up when reading them.
type TimestampWriter struct {
	writer io.Writer
	format string
}

func NewTimestampWriter(writer io.Writer) *TimestampWriter {
	return &TimestampWriter{writer, "15:04:05.000000 "}
}

func (w *TimestampWriter) Write(data []byte) (int, error) {
	return w.writer.Write(append([]byte(time.Now().Format(w.format)), data...))
}

// NewStdioWritersWithTimestamp returns the same writers as NewStdioWriters() but prepends
// each line it writes with the host timestamp.
func NewStdioWritersWithTimestamp(ctx context.Context) (io.Writer, io.Writer, func()) {
	return newStdioWriters(ctx, NewTimestampWriter(streams.Stdout(ctx)), NewTimestampWriter(streams.Stderr(ctx)))
}

// NewStiodWriters returns a new LineWriter for the stdout and stderr associated
// with the provided context. It also returns a function to flush out any
// remaining data not written by Write because it didn't end with a newline.
// This should be used instead of the WithTimestamp version when a timestamp
// is not needed, for example, when used with a logger which already prepends
// each log with a timestamp.
func NewStdioWriters(ctx context.Context) (io.Writer, io.Writer, func()) {
	return newStdioWriters(ctx, streams.Stdout(ctx), streams.Stderr(ctx))
}

func newStdioWriters(ctx context.Context, stdout, stderr io.Writer) (io.Writer, io.Writer, func()) {
	stdoutWriter := NewLineWriter(stdout)
	stderrWriter := NewLineWriter(stderr)
	flush := func() {
		// Flush out the rest of the data stored by the writers.
		if len(stdoutWriter.line) > 0 {
			if _, err := stdoutWriter.Write([]byte("\n")); err != nil {
				logger.Debugf(ctx, "failed to flush out data to stdout %q: %s", string(stdoutWriter.line), err)
			}
		}
		if len(stderrWriter.line) > 0 {
			if _, err := stderrWriter.Write([]byte("\n")); err != nil {
				logger.Debugf(ctx, "failed to flush out data to stderr %q: %s", string(stderrWriter.line), err)
			}
		}
	}
	return stdoutWriter, stderrWriter, flush
}
