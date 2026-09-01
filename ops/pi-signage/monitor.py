#!/usr/bin/env python3
"""Local-only operations panel for the Lait Signage Pi qualification run."""

from __future__ import annotations

import argparse
import base64
import concurrent.futures
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.request import urlopen


REMOTE_SAMPLE = r"""
value() { printf '%s=%s\n' "$1" "$2"; }
service() { systemctl is-active "$1" 2>/dev/null || true; }
encode() { base64 -w0 2>/dev/null || base64; }
value hostname "$(hostname)"
value addresses "$(hostname -I 2>/dev/null | xargs)"
value boot_id "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
value uptime_s "$(cut -d. -f1 /proc/uptime 2>/dev/null)"
value load_1 "$(awk '{print $1}' /proc/loadavg 2>/dev/null)"
value temp_mc "$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo 0)"
value memory_total_kb "$(awk '/MemTotal:/{print $2}' /proc/meminfo)"
value memory_available_kb "$(awk '/MemAvailable:/{print $2}' /proc/meminfo)"
value root_used_pct "$(df -P / 2>/dev/null | awk 'NR==2{gsub(/%/,"",$5); print $5}')"
value cloud_final "$(service cloud-final.service)"
value provision "$(service lait-pi-provision.service)"
value lait "$(service lait.service)"
value receiver "$(service lait-display-receiver.service)"
value ssh "$(service ssh.service)"
value wayvnc "$(if ss -ltn 2>/dev/null | grep -q '127.0.0.1:5900'; then echo listening; else echo waiting; fi)"
value bootstrap "$([ -s /boot/firmware/signage-bootstrap.json ] && echo present || echo waiting)"
value handoff "$([ -s /var/lib/astrolabe-display/output/active.json ] && echo present || echo waiting)"
value provision_log_b64 "$(sudo -n journalctl -u lait-pi-provision -n 45 --no-pager -o short-iso 2>/dev/null | encode)"
value lait_log_b64 "$(sudo -n journalctl -u lait -n 45 --no-pager -o short-iso 2>/dev/null | encode)"
value receiver_log_b64 "$(sudo -n journalctl -u lait-display-receiver -n 45 --no-pager -o short-iso 2>/dev/null | encode)"
value handoff_b64 "$([ -s /var/lib/astrolabe-display/output/active.json ] && encode < /var/lib/astrolabe-display/output/active.json || true)"
"""


def now_ms() -> int:
    return int(time.time() * 1000)


def run(command: list[str], *, timeout: float = 8, input_text: str | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        command,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


class Monitor:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.root = Path(__file__).resolve().parent
        self.cache = self.root / ".cache"
        self.cache.mkdir(parents=True, exist_ok=True)
        self.known_hosts = self.cache / "monitor-known-hosts"
        # macOS caps Unix-domain socket paths at 104 bytes. A workspace path
        # can cross that limit before OpenSSH appends its connection hash.
        self.control_path = f"/tmp/lait-pi-ssh-{os.getuid()}-%C"
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.host = args.host
        self.tunnel: subprocess.Popen | None = None
        self.state: dict = {
            "phase": "discovering",
            "online": False,
            "host": self.host,
            "user": args.user,
            "message": "Waiting for the Pi to accept the operator SSH key",
            "sampled_at": None,
            "started_at": now_ms(),
            "vnc_tunnel": "stopped",
            "services": {},
            "metrics": {},
            "logs": {},
            "handoff": None,
            "coordinator": None,
        }

    def ssh_base(self, host: str) -> list[str]:
        return [
            "ssh",
            "-i",
            os.path.expanduser(self.args.identity),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            f"UserKnownHostsFile={self.known_hosts}",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=45",
            "-o",
            f"ControlPath={self.control_path}",
            f"{self.args.user}@{host}",
        ]

    def probe(self, host: str) -> bool:
        result = run(self.ssh_base(host) + ["printf '__LAIT_PI__'"], timeout=5)
        return result.returncode == 0 and result.stdout == "__LAIT_PI__"

    @staticmethod
    def local_network() -> tuple[str, int] | None:
        for interface in ("en0", "en1"):
            address = run(["ipconfig", "getifaddr", interface], timeout=2).stdout.strip()
            mask = run(["ipconfig", "getoption", interface, "subnet_mask"], timeout=2).stdout.strip()
            if not address or mask != "255.255.255.0":
                continue
            return address.rsplit(".", 1)[0], int(address.rsplit(".", 1)[1])
        return None

    @staticmethod
    def port_open(host: str, port: int, timeout: float = 0.22) -> bool:
        try:
            with socket.create_connection((host, port), timeout=timeout):
                return True
        except OSError:
            return False

    def discover(self) -> str | None:
        if self.host and self.probe(self.host):
            return self.host
        local = self.local_network()
        if not local:
            return None
        prefix, own = local
        addresses = [f"{prefix}.{last}" for last in range(1, 255) if last != own]
        with concurrent.futures.ThreadPoolExecutor(max_workers=48) as pool:
            open_hosts = [
                host
                for host, opened in zip(addresses, pool.map(lambda item: self.port_open(item, 22), addresses))
                if opened
            ]
        for host in open_hosts:
            if self.probe(host):
                return host
        return None

    @staticmethod
    def decode(value: str) -> str:
        if not value:
            return ""
        try:
            return base64.b64decode(value).decode("utf-8", errors="replace")
        except Exception:
            return ""

    @staticmethod
    def number(values: dict[str, str], key: str, default: float = 0) -> float:
        try:
            return float(values.get(key, default))
        except (TypeError, ValueError):
            return default

    def coordinator(self, host: str) -> dict | None:
        context = ssl._create_unverified_context()
        try:
            with urlopen(f"https://{host}:7443/head/v1/instance", timeout=3, context=context) as response:
                return json.load(response)
        except (OSError, URLError, ValueError):
            return None

    def sample(self, host: str) -> dict:
        result = run(self.ssh_base(host) + ["bash -s"], timeout=10, input_text=REMOTE_SAMPLE)
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or f"SSH exited {result.returncode}")
        values: dict[str, str] = {}
        for line in result.stdout.splitlines():
            key, separator, value = line.partition("=")
            if separator:
                values[key] = value
        total = self.number(values, "memory_total_kb")
        available = self.number(values, "memory_available_kb")
        used_pct = round(((total - available) / total * 100), 1) if total else 0
        temp_c = round(self.number(values, "temp_mc") / 1000, 1)
        handoff_text = self.decode(values.get("handoff_b64", ""))
        try:
            handoff = json.loads(handoff_text) if handoff_text else None
        except ValueError:
            handoff = {"unreadable": handoff_text}
        return {
            "phase": "online",
            "online": True,
            "host": host,
            "user": self.args.user,
            "hostname": values.get("hostname", "unknown"),
            "addresses": values.get("addresses", ""),
            "boot_id": values.get("boot_id", ""),
            "message": "Pi reachable; telemetry current",
            "sampled_at": now_ms(),
            "services": {
                "cloud-init": values.get("cloud_final", "unknown"),
                "provisioning": values.get("provision", "unknown"),
                "lait": values.get("lait", "unknown"),
                "receiver": values.get("receiver", "unknown"),
                "ssh": values.get("ssh", "unknown"),
                "remote display": values.get("wayvnc", "unknown"),
            },
            "metrics": {
                "uptime_s": self.number(values, "uptime_s"),
                "load_1": self.number(values, "load_1"),
                "temp_c": temp_c,
                "memory_used_pct": used_pct,
                "root_used_pct": self.number(values, "root_used_pct"),
            },
            "receiver": {
                "bootstrap": values.get("bootstrap", "unknown"),
                "handoff": values.get("handoff", "unknown"),
            },
            "handoff": handoff,
            "logs": {
                "provisioning": self.decode(values.get("provision_log_b64", "")),
                "lait": self.decode(values.get("lait_log_b64", "")),
                "receiver": self.decode(values.get("receiver_log_b64", "")),
            },
            "coordinator": self.coordinator(host),
        }

    def tunnel_state(self) -> str:
        if self.tunnel is None:
            return "stopped"
        code = self.tunnel.poll()
        if code is None:
            return "listening"
        self.tunnel = None
        return f"stopped ({code})"

    def poll(self) -> None:
        while not self.stop.is_set():
            try:
                host = self.discover()
                if not host:
                    with self.lock:
                        self.state.update(
                            phase="discovering",
                            online=False,
                            host=None,
                            message="Scanning the local /24 for the operator SSH key",
                            sampled_at=now_ms(),
                            vnc_tunnel=self.tunnel_state(),
                        )
                    self.stop.wait(3)
                    continue
                self.host = host
                sample = self.sample(host)
                with self.lock:
                    started = self.state.get("started_at")
                    self.state = {**sample, "started_at": started, "vnc_tunnel": self.tunnel_state()}
            except Exception as error:
                with self.lock:
                    self.state.update(
                        phase="stale",
                        online=False,
                        message=str(error),
                        sampled_at=now_ms(),
                        vnc_tunnel=self.tunnel_state(),
                    )
            self.stop.wait(self.args.interval)

    def snapshot(self) -> dict:
        with self.lock:
            return json.loads(json.dumps(self.state))

    def rediscover(self) -> None:
        self.host = self.args.host
        with self.lock:
            self.state.update(phase="discovering", online=False, message="Discovery requested")

    def start_tunnel(self) -> tuple[bool, str]:
        host = self.host
        if not host:
            return False, "Pi has not been discovered"
        if self.tunnel and self.tunnel.poll() is None:
            return True, f"VNC tunnel already listening on {self.args.vnc_port}"
        command = self.ssh_base(host)
        destination = command.pop()
        command += [
            "-o",
            "ExitOnForwardFailure=yes",
            "-N",
            "-L",
            f"127.0.0.1:{self.args.vnc_port}:127.0.0.1:5900",
            destination,
        ]
        self.tunnel = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        time.sleep(0.8)
        code = self.tunnel.poll()
        if code is not None:
            message = self.tunnel.stderr.read().strip() if self.tunnel.stderr else ""
            self.tunnel = None
            return False, message or f"SSH tunnel exited {code}"
        return True, f"VNC tunnel listening on 127.0.0.1:{self.args.vnc_port}"

    def frame(self) -> bytes | None:
        if not self.host:
            return None
        result = subprocess.run(
            self.ssh_base(self.host) + ["cat /var/lib/astrolabe-display/output/frame.png"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        )
        return result.stdout if result.returncode == 0 and result.stdout.startswith(b"\x89PNG") else None


def handler_for(monitor: Monitor):
    panel = (Path(__file__).resolve().parent / "panel.html").read_bytes()

    class Handler(BaseHTTPRequestHandler):
        server_version = "LaitPiPanel/1"

        def log_message(self, format: str, *args) -> None:
            return

        def send_bytes(self, body: bytes, content_type: str, status: HTTPStatus = HTTPStatus.OK) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(body)

        def send_json(self, value: dict, status: HTTPStatus = HTTPStatus.OK) -> None:
            self.send_bytes(json.dumps(value).encode(), "application/json; charset=utf-8", status)

        def do_GET(self) -> None:
            path = self.path.split("?", 1)[0]
            if path == "/":
                self.send_bytes(panel, "text/html; charset=utf-8")
            elif path == "/api/status":
                self.send_json(monitor.snapshot())
            elif path == "/api/frame":
                frame = monitor.frame()
                if frame:
                    self.send_bytes(frame, "image/png")
                else:
                    self.send_bytes(b"", "image/png", HTTPStatus.NOT_FOUND)
            else:
                self.send_bytes(b"not found", "text/plain", HTTPStatus.NOT_FOUND)

        def do_POST(self) -> None:
            path = self.path.split("?", 1)[0]
            if path == "/api/discover":
                monitor.rediscover()
                self.send_json({"ok": True, "message": "Discovery restarted"})
            elif path == "/api/vnc":
                ok, message = monitor.start_tunnel()
                self.send_json({"ok": ok, "message": message}, HTTPStatus.OK if ok else HTTPStatus.CONFLICT)
            else:
                self.send_json({"ok": False, "message": "not found"}, HTTPStatus.NOT_FOUND)

    return Handler


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", help="Pi IP or hostname; otherwise discover by SSH key")
    parser.add_argument("--user", default="operator")
    parser.add_argument("--identity", default="~/.ssh/id_ed25519")
    parser.add_argument("--listen", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--vnc-port", type=int, default=5900)
    parser.add_argument("--interval", type=float, default=2.5)
    parser.add_argument("--open", action="store_true", dest="open_browser")
    args = parser.parse_args()
    identity = Path(os.path.expanduser(args.identity))
    if not identity.is_file():
        parser.error(f"SSH identity does not exist: {identity}")

    monitor = Monitor(args)
    poller = threading.Thread(target=monitor.poll, name="pi-poller", daemon=True)
    poller.start()
    server = ThreadingHTTPServer((args.listen, args.port), handler_for(monitor))
    url = f"http://{args.listen}:{args.port}"
    print(f"Lait Pi panel: {url}", flush=True)
    if args.open_browser:
        subprocess.Popen(["open", url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        monitor.stop.set()
        if monitor.tunnel and monitor.tunnel.poll() is None:
            monitor.tunnel.terminate()
        server.server_close()


if __name__ == "__main__":
    main()
