#!/usr/bin/env python3
"""Send one pacing control and confirm applied state independently of the log writer."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("command", choices=("status", "mark", "record", "direct"))
parser.add_argument("value", nargs="?")
parser.add_argument("--pid", type=int)
args = parser.parse_args()
if args.command == "mark" and args.value is not None:
    args.value = args.value.strip()
if args.command in ("record", "direct") and args.value not in ("on", "off"):
    parser.error("record/direct require on or off")
if args.command == "status" and args.value is not None:
    parser.error("status takes no value")
if args.command == "mark" and (not args.value or len(args.value) > 80 or
        not all(c.isascii() and (c.isalnum() or c in "_- .") for c in args.value)):
    parser.error("labels use up to 80 ASCII letters/numbers, spaces, underscore, dash or dot")
pid = args.pid or int(subprocess.check_output(
    ["systemctl", "--user", "show", "niri.service", "-p", "MainPID", "--value"], text=True).strip())
if pid <= 0:
    parser.error("niri.service is not running")
runtime = Path(os.environ["XDG_RUNTIME_DIR"])
path = runtime / f"niri-pacing-{pid}.sock"
command = args.command if args.value is None else f"{args.command} {args.value}"
with tempfile.TemporaryDirectory(prefix="niri-pacing-client-", dir=runtime) as directory:
    local = str(Path(directory) / "reply")
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as control:
        control.bind(local)
        os.chmod(local, 0o600)
        control.settimeout(5)
        control.connect(str(path))
        control.sendall(command.encode())
        try:
            response = json.loads(control.recv(4096))
        except socket.timeout:
            raise SystemExit("No reply: the command MAY have applied. Query status; do not retry mutations blindly.")
if response.get("pid") != pid or response.get("command") != command:
    raise SystemExit(f"Unexpected acknowledgment: {response!r}")
print(json.dumps(response, sort_keys=True))
if not response.get("accepted"):
    raise SystemExit(1)
