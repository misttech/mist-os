// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package resultdb

import (
	"flag"
	"os"
	"path/filepath"
	"testing"

	resultpb "go.chromium.org/luci/resultdb/proto/v1"
	sinkpb "go.chromium.org/luci/resultdb/sink/proto/v1"
)

var testDataDir = flag.String("test_data_dir", "testdata", "Path to testdata/; only used in GN build")

func TestGetLUCICtx(t *testing.T) {
	old := os.Getenv("LUCI_CONTEXT")
	defer os.Setenv("LUCI_CONTEXT", old)
	os.Setenv("LUCI_CONTEXT", filepath.Join(*testDataDir, "lucictx.json"))
	client, err := NewClient()
	if err != nil {
		t.Errorf("Cannot parse LUCI_CONTEXT: %v", err)
	}
	if client.resultSink.ResultSinkAddr != "result.sink" {
		t.Errorf("Incorrect value parsed for result_sink address. Got %s", client.resultSink.ResultSinkAddr)
	}
	if client.resultSink.AuthToken != "token" {
		t.Errorf("Incorrect value parsed for result_sink auth_token field. Got %s", client.resultSink.AuthToken)
	}
}

func TestParse2Summary(t *testing.T) {
	t.Parallel()
	const chunkSize = 5
	var requests []*sinkpb.ReportTestResultsRequest
	expectRequests := 0
	for _, name := range []string{"summary.json", "summary2.json"} {
		summary, err := ParseSummary(filepath.Join(*testDataDir, name))
		if err != nil {
			t.Fatal(err)
		}
		testResults, skipped := SummaryToResultSink(summary, []*resultpb.StringPair{}, name)
		expectRequests += (len(testResults)-1)/chunkSize + 1
		requests = append(requests, createTestResultsRequests(testResults, chunkSize)...)
		for _, testResult := range testResults {
			if len(testResult.TestId) == 0 {
				t.Errorf("Empty testId is not allowed.")
			}
		}
		if len(skipped) != 0 {
			t.Errorf("Tests got skipped %v, expect no skip", skipped)
		}
	}
	if len(requests) != expectRequests {
		t.Errorf("Incorrect number of request chuncks, got: %d want %d", len(requests), expectRequests)
	}
}

func TestFailWithLongTestName(t *testing.T) {
	summary, err := ParseSummary(filepath.Join(*testDataDir, "summary_long_name.json"))
	if err != nil {
		t.Fatal(err)
	}
	_, testsSkipped := SummaryToResultSink(summary, []*resultpb.StringPair{}, "")

	skippedTestName := "fuchsia-pkg://fuchsia.com/netstack-integration-tests#meta/netstack-inspect-integration-test.cm/:fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_fuchsia_inspect_dhcp_netdevice::_multiple_invalid_port_and_single_invalid_trans_proto_vec_packetattributes_ip_proto_packet_formats_ip_ipv4proto_proto_packet_formats_ip_ipproto_udp_port_invalid_port_packetattributes_ip_proto_packet_formats_ip_ipv4proto_proto_packet_formats_ip_ipproto_udp_port_invalid_port_packetattributes_ip_proto_packet_formats_ip_ipv4proto_proto_packet_formats_ip_ipproto_tcp_port_dhcp_client_port_"

	if len(testsSkipped) != 1 {
		t.Errorf("Incorrect number of skipped tests got: %d want %d", len(testsSkipped), 1)
	}
	if testsSkipped[0] != skippedTestName {
		t.Errorf("Incorrect skipped test, got: %s want %s", testsSkipped[0], skippedTestName)
	}
}
