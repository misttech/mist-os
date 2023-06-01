# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import argparse
import json
import os
import queue
import subprocess

from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import List

rule_table = {}
trigger_queue = queue.Queue()


class PubSubHandler(BaseHTTPRequestHandler):

    def do_GET(self):
        event = self.path.strip('/')  # Extract event from the URL

        trigger_queue.put(event)
        self.send_response(200)
        self.end_headers()
        trigger_events()


def execute_commands_in_repo(repo: str, commands: List[str]):
    print(f'Run in repo: {repo}')
    for command in commands:
        print(f'Execute command: {command}')
        cmd = command.split()
        result = subprocess.run(cmd, cwd=os.path.expanduser(repo))


def trigger_events():
    while not trigger_queue.empty():
        event = trigger_queue.get()
        print(f'Event happened: {event}')
        if event not in rule_table:
            print(f'Skipping not defined event: {event}')
            continue
        triggered_action = rule_table[event]
        triggered_repo = triggered_action['repo']
        triggered_commands = triggered_action['commands']
        execute_commands_in_repo(triggered_repo, triggered_commands)
        [trigger_queue.put(e) for e in triggered_action['triggered_events']]


def parse_config(config_path: str):
    if not config_path:
        return
    global rule_table
    with open(config_path, 'r') as f:
        rule_table = json.load(f)


def run_server():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--config',
        help='A path to the config file.',
    )
    args = parser.parse_args()

    # Parse configuration file
    parse_config(args.config)

    # Start server
    server_address = ('localhost', 8000)
    httpd = HTTPServer(server_address, PubSubHandler)
    print('Starting server...')
    httpd.serve_forever()


if __name__ == '__main__':
    run_server()
