// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package syslog_test

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall/zx"
	"syscall/zx/fidl"
	"syscall/zx/zxwait"
	"testing"
	"unicode/utf8"

	"fidl/fuchsia/diagnostics"
	"fidl/fuchsia/logger"

	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"
)

const format = "integer: %d"

var pid = uint64(os.Getpid())

var _ logger.LogSinkWithCtx = (*logSinkImpl)(nil)

type logSinkImpl struct {
	onConnect             func(fidl.Context, zx.Socket)
	waitForInterestChange <-chan logger.LogSinkWaitForInterestChangeResult
}

func (*logSinkImpl) Connect(fidl.Context, zx.Socket) error {
	return nil
}

func (impl *logSinkImpl) ConnectStructured(ctx fidl.Context, socket zx.Socket) error {
	impl.onConnect(ctx, socket)
	return nil
}

func (impl *logSinkImpl) WaitForInterestChange(fidl.Context) (logger.LogSinkWaitForInterestChangeResult, error) {
	if result := <-impl.waitForInterestChange; result != (logger.LogSinkWaitForInterestChangeResult{}) {
		return result, nil
	}
	return logger.LogSinkWaitForInterestChangeResult{}, &zx.Error{Status: zx.ErrCanceled}
}

func TestLogSimple(t *testing.T) {
	actual := bytes.Buffer{}
	var options syslog.LogInitOptions
	options.MinSeverityForFileAndLineInfo = syslog.ErrorLevel
	options.Writer = &actual
	log, err := syslog.NewLogger(options)
	if err != nil {
		t.Fatal(err)
	}
	if err := log.Infof(format, 10); err != nil {
		t.Fatal(err)
	}
	expected := "INFO: integer: 10\n"
	got := string(actual.Bytes())
	if !strings.HasSuffix(got, expected) {
		t.Errorf("%q should have ended in %q", got, expected)
	}
	if !strings.Contains(got, fmt.Sprintf("[%d]", pid)) {
		t.Errorf("%q should contains %d", got, pid)
	}
}

func setup(t *testing.T, tags ...string) (chan<- logger.LogSinkWaitForInterestChangeResult, zx.Socket, *syslog.Logger) {
	req, logSink, err := logger.NewLogSinkWithCtxInterfaceRequest()
	if err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	ch := make(chan struct{})
	t.Cleanup(func() {
		cancel()
		<-ch
	})

	waitForInterestChange := make(chan logger.LogSinkWaitForInterestChangeResult)
	t.Cleanup(func() { close(waitForInterestChange) })

	sinChan := make(chan zx.Socket, 1)
	defer close(sinChan)
	go func() {
		defer close(ch)

		component.Serve(ctx, &logger.LogSinkWithCtxStub{
			Impl: &logSinkImpl{
				onConnect: func(_ fidl.Context, socket zx.Socket) {
					sinChan <- socket
				},
				waitForInterestChange: waitForInterestChange,
			},
		}, req.Channel, component.ServeOptions{
			OnError: func(err error) {
				var zxError *zx.Error
				if errors.As(err, &zxError) {
					if zxError.Status == zx.ErrCanceled {
						return
					}
				}
				t.Error(err)
			},
		})
	}()

	options := syslog.LogInitOptions{
		LogLevel: syslog.InfoLevel,
	}
	options.LogSink = logSink
	options.MinSeverityForFileAndLineInfo = syslog.ErrorLevel
	options.Tags = tags
	log, err := syslog.NewLogger(options)
	if err != nil {
		t.Fatal(err)
	}

	s := <-sinChan

	// Throw away system-generated messages.
	for i := 0; i < 1; i++ {
		if _, err := zxwait.WaitContext(context.Background(), zx.Handle(s), zx.SignalSocketReadable); err != nil {
			t.Fatal(err)
		}
		var data [logger.MaxDatagramLenBytes]byte
		if _, err := s.Read(data[:], 0); err != nil {
			t.Fatal(err)
		}
	}

	return waitForInterestChange, s, log
}

func decodeStringArg(t *testing.T, data []byte) (string, string, int) {
	header := binary.LittleEndian.Uint64(data[0:8])
	if header&0xffff_8000_8000_000f != 0x0000_8000_8000_0006 {
		t.Fatalf("got bad string arg header: %x", header)
	}
	wordCount := header >> 4 & 0xfff
	nameLen := (header >> 16) & 0x7fff
	valueLen := (header >> 32) & 0x7fff
	if wordCount != 1+(nameLen+7)/8+(valueLen+7)/8 {
		t.Fatalf("bad word count: %x", header)
	}
	roundedNameLen := (nameLen + 7) &^ 7
	return string(data[8 : 8+nameLen]), string(data[8+roundedNameLen : 8+roundedNameLen+valueLen]), int(wordCount * 8)
}

func checkoutput(t *testing.T, sin zx.Socket, expectedMsg string, severity syslog.LogLevel, tags ...string) {
	var data [logger.MaxDatagramLenBytes]byte
	n, err := sin.Read(data[:], 0)
	if err != nil {
		t.Fatal(err)
	}
	if n <= 32 {
		t.Fatalf("got invalid data: %x", data[:n])
	}

	header := binary.LittleEndian.Uint64(data[0:8])
	gotSeverity := int32(header >> 56)

	if int32(severity) != gotSeverity {
		t.Errorf("severity error, got: %d, want: %d", gotSeverity, severity)
	}

	gotTime := binary.LittleEndian.Uint64(data[8:16])
	if gotTime <= 0 {
		t.Errorf("time %d should be greater than zero", gotTime)
	}

	// This only works for arguments that have a 3 byte name
	const expectedUint64ArgHeader = 4 | 3<<4 | 0x8003<<16

	pidArgHeader := binary.LittleEndian.Uint64(data[16:24])
	if pidArgHeader != expectedUint64ArgHeader {
		t.Errorf("expected pid arg header, got: %x, want: %x", pidArgHeader, expectedUint64ArgHeader)
	}
	pidArgName := binary.LittleEndian.Uint64(data[24:32])
	if pidArgName != binary.LittleEndian.Uint64([]byte{'p', 'i', 'd', 0, 0, 0, 0, 0}) {
		t.Errorf("expected pid arg name, got: %x", pidArgName)
	}
	gotPid := binary.LittleEndian.Uint64(data[32:40])

	if pid != gotPid {
		t.Errorf("pid error, got: %d, want: %d", gotPid, pid)
	}

	tidArgHeader := binary.LittleEndian.Uint64(data[40:48])
	if tidArgHeader != expectedUint64ArgHeader {
		t.Errorf("expected tid arg header, got: %x, want: %x", tidArgHeader, expectedUint64ArgHeader)
	}
	tidArgName := binary.LittleEndian.Uint64(data[48:56])
	if tidArgName != binary.LittleEndian.Uint64([]byte{'t', 'i', 'd', 0, 0, 0, 0, 0}) {
		t.Errorf("expected tid arg name, got: %x", tidArgName)
	}
	gotTid := binary.LittleEndian.Uint64(data[56:64])

	if 0 != gotTid {
		t.Errorf("tid error, got: %d, want: %d", gotTid, 0)
	}

	// If there were any dropped logs, that would follow, but we don't expect any dropped logs so just continue and we'll fail later if the dropped argument is present
	var pos = 64
	for i, tag := range tags {
		length := len(tag)

		name, gotTag, length := decodeStringArg(t, data[pos:])
		if name != "tag" {
			t.Errorf("expected tag header, got: %s", name)
		}
		if tag != gotTag {
			t.Fatalf("tag iteration %d: expected tag %q, got %q", i, tag, gotTag)
		}
		pos += length
	}

	name, msgGot, length := decodeStringArg(t, data[pos:])

	if name != "message" {
		t.Fatalf("expected message argument, got '%s' argument", name)
	}
	if expectedMsg != msgGot {
		t.Fatalf("expected msg:%q, got %q", expectedMsg, msgGot)
	}
	pos += length
	if pos != n {
		t.Fatalf("extra data in log message: %x", data[pos:n])
	}
}

func TestLog(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()

	if err := log.Infof(format, 10); err != nil {
		t.Fatal(err)
	}
	checkoutput(t, sin, fmt.Sprintf(format, 10), syslog.InfoLevel)
}

func TestLogWithLocalTag(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := log.InfoTf("local_tag", format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel, "local_tag")
}

func TestLogWithGlobalTags(t *testing.T) {
	_, sin, log := setup(t, "gtag1", "gtag2")
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := log.InfoTf("local_tag", format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel, "gtag1", "gtag2", "local_tag")
}

func TestLoggerSeverity(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	log.SetSeverity(diagnostics.Severity(syslog.WarningLevel))
	if err := log.Infof(format, 10); err != nil {
		t.Fatal(err)
	}
	_, err := sin.Read(nil, 0)
	if err, ok := err.(*zx.Error); !ok || err.Status != zx.ErrShouldWait {
		t.Fatal(err)
	}
	if err := log.Warnf(format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.WarningLevel)
}

func TestLoggerVerbosity(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := log.VLogf(syslog.DebugVerbosity, format, 10); err != nil {
		t.Fatal(err)
	}
	_, err := sin.Read(nil, 0)
	if err, ok := err.(*zx.Error); !ok || err.Status != zx.ErrShouldWait {
		t.Fatal(err)
	}
	log.SetVerbosity(syslog.DebugVerbosity)
	if err := log.VLogf(syslog.DebugVerbosity, format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel-1)
}

func TestLoggerRegisterInterest(t *testing.T) {
	ch, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()

	registerInterest := func(interest diagnostics.Interest) {
		t.Helper()

		ch <- logger.LogSinkWaitForInterestChangeResultWithResponse(logger.LogSinkWaitForInterestChangeResponse{
			Data: interest,
		})

		// Consume the system-generated messages.
		for i := 0; i < 2; i++ {
			if _, err := zxwait.WaitContext(context.Background(), zx.Handle(sin), zx.SignalSocketReadable); err != nil {
				t.Fatal(err)
			}
			var data [logger.MaxDatagramLenBytes]byte
			if _, err := sin.Read(data[:], 0); err != nil {
				t.Fatal(err)
			}
		}
	}

	// Register interest and observe that the log is emitted.
	{
		var interest diagnostics.Interest
		interest.SetMinSeverity(diagnostics.SeverityDebug)
		registerInterest(interest)
	}
	if err := log.VLogf(syslog.DebugVerbosity, format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel-1)

	// Register empty interest and observe that severity resets to initial.
	registerInterest(diagnostics.Interest{})
	if err := log.VLogf(syslog.DebugVerbosity, format, 10); err != nil {
		t.Fatal(err)
	}
	_, err := sin.Read(nil, 0)
	if err, ok := err.(*zx.Error); !ok || err.Status != zx.ErrShouldWait {
		t.Fatal(err)
	}
}

func TestGlobalTagLimits(t *testing.T) {
	var options syslog.LogInitOptions
	options.Writer = os.Stdout
	var tags [logger.MaxTags + 1]string
	for i := 0; i < len(tags); i++ {
		tags[i] = "a"
	}
	options.Tags = tags[:]
	if _, err := syslog.NewLogger(options); err == nil || !strings.Contains(err.Error(), "too many tags") {
		t.Fatalf("unexpected error: %s", err)
	}
	options.Tags = tags[:logger.MaxTags]
	var tag [logger.MaxTagLenBytes + 1]byte
	for i := 0; i < len(tag); i++ {
		tag[i] = 65
	}
	options.Tags[1] = string(tag[:])
	if _, err := syslog.NewLogger(options); err == nil || !strings.Contains(err.Error(), "tag too long") {
		t.Fatalf("unexpected error: %s", err)
	}
}

func TestLocalTagLimits(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	var tag [logger.MaxTagLenBytes + 1]byte
	for i := 0; i < len(tag); i++ {
		tag[i] = 65
	}
	if err := log.InfoTf(string(tag[:]), format, 10); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf(format, 10)
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel, string(tag[:logger.MaxTagLenBytes]))
}

func TestLogToWriterWhenSocketCloses(t *testing.T) {
	_, sin, log := setup(t, "gtag1", "gtag2")
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := sin.Close(); err != nil {
		t.Fatal(err)
	}
	old := os.Stderr
	defer func() {
		os.Stderr = old
	}()

	f, err := os.Create(filepath.Join(t.TempDir(), "syslog-test"))
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := f.Close(); err != nil {
			t.Error(err)
		}
	}()
	os.Stderr = f
	if err := log.InfoTf("local_tag", format, 10); err != nil {
		t.Fatal(err)
	}
	if err := f.Sync(); err != nil {
		t.Fatal(err)
	}
	expectedMsg := fmt.Sprintf("[0][gtag1, gtag2, local_tag] INFO: %s\n", fmt.Sprintf(format, 10))
	content, err := os.ReadFile(f.Name())
	os.Stderr = old
	if err != nil {
		t.Fatal(err)
	}
	got := string(content)
	if !strings.HasSuffix(got, expectedMsg) {
		t.Fatalf("%q should have ended in %q", got, expectedMsg)
	}
}

func TestMessageLenLimit(t *testing.T) {
	_, sin, log := setup(t)
	defer func() {
		if err := log.Close(); err != nil {
			t.Error(err)
		}
		if err := sin.Close(); err != nil {
			t.Error(err)
		}
	}()
	// Without tags, it's 64 bytes to the message argument plus 16 bytes for the message argument header
	msgLen := int(logger.MaxDatagramLenBytes) - 80

	const stripped = '𠜎'
	// Ensure only part of stripped fits.
	msg := strings.Repeat("x", msgLen-(utf8.RuneLen(stripped)-1)) + string(stripped)
	switch err := log.Infof(msg).(type) {
	case *syslog.ErrMsgTooLong:
		if err.Msg != string(stripped) {
			t.Fatalf("unexpected truncation: %s", err.Msg)
		}
	default:
		t.Fatalf("unexpected error: %#v", err)
	}

	const ellipsis = "..."
	expectedMsg := msg[:msgLen-len(ellipsis)] + ellipsis
	checkoutput(t, sin, expectedMsg, syslog.InfoLevel)
}
