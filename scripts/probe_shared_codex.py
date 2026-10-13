"""Probe shared app-server using unauthenticated, loopback-only mock responses."""

import argparse
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import threading
import time

try:
    import websockets
    from websockets.legacy.client import connect
except ImportError:
    websockets = None
    connect = None


ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {"initialize", "thread/start", "thread/resume", "thread/read", "mcpServerStatus/list", "turn/start", "turn/interrupt"}
FIXTURE = {"image_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
           "document": {"documentId": 1, "baseRevision": 1, "canvas": [1, 1], "hasSelection": False, "area": [0, 0, 1, 1]}}


def arguments():
    parser = argparse.ArgumentParser(
        description="Probe Codex shared-thread behavior with local mock responses only; no real model calls or user configuration writes.",
        epilog="Requires Python 3.10+, websockets (tested with 13.1), a Codex CLI with authenticated WebSocket app-server support, "
               "and a built PhotoCraft helper supporting --ai-canvas-mcp. Uses a temporary CODEX_HOME without account credentials.")
    parser.add_argument("--codex", metavar="EXECUTABLE", default=shutil.which("codex"),
                        help="Codex CLI executable (default: discover codex on PATH)")
    parser.add_argument("--photocraft", metavar="EXECUTABLE",
                        default=str(ROOT / "target" / "debug" / ("photocraft.exe" if os.name == "nt" else "photocraft")),
                        help="PhotoCraft MCP helper (default: this repository's target/debug/photocraft[.exe])")
    parser.add_argument("--mock-only", action="store_true", default=True,
                        help="Use only the loopback mock provider (always enabled; real-provider mode is not available)")
    args = parser.parse_args()
    if websockets is None:
        parser.error("Missing dependency: install websockets (tested with version 13.1)")
    for name in ("codex", "photocraft"):
        value = getattr(args, name)
        resolved = (shutil.which(value) or value) if value else None
        if not resolved or not Path(resolved).is_file():
            parser.error(f"Cannot find {name} executable; pass --{name} with its path")
        setattr(args, name, Path(resolved).resolve())
    return args


class MockResponses(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    hold = threading.Event()
    entered = threading.Event()
    requests = []

    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_error(404)

    def output_item(self, request):
        return {"id": "msg_probe", "type": "message", "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": "Local mock complete.", "annotations": []}]}

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        self.requests.append(request)
        self.entered.set()
        if not self.hold.wait(30):
            self.send_error(504)
            return
        item = self.output_item(request)
        response = {"id": "resp_probe", "object": "response", "created_at": int(time.time()), "status": "completed",
                    "model": request.get("model", "probe-model"), "output": [item],
                    "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}
        added = dict(item, status="in_progress")
        if item["type"] == "message":
            added["content"] = []
        events = [{"type": "response.created", "response": dict(response, status="in_progress", output=[])},
                  {"type": "response.output_item.added", "output_index": 0, "item": added},
                  {"type": "response.output_item.done", "output_index": 0, "item": item},
                  {"type": "response.completed", "response": response}]
        payload = "".join("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


class Client:
    def __init__(self, connection):
        self.connection = connection
        self.serial = 0
        self.notifications = []

    async def call(self, method, params):
        if method not in ALLOWED:
            raise ValueError("Probe does not permit this method")
        self.serial += 1
        print("CALL", method, flush=True)
        request_id = self.serial
        await self.connection.send(json.dumps({"id": request_id, "method": method, "params": params}))
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            event = json.loads(await asyncio.wait_for(self.connection.recv(), deadline - time.monotonic()))
            if event.get("id") == request_id and "method" not in event:
                return event
            self.notifications.append(event)
        raise TimeoutError(method)

    async def completed(self, turn_id):
        while True:
            event = json.loads(await asyncio.wait_for(self.connection.recv(), 40))
            self.notifications.append(event)
            if event.get("method") == "turn/completed" and event["params"]["turn"]["id"] == turn_id:
                return event

    async def initialize(self, name):
        reply = await self.call("initialize", {"clientInfo": {"name": name, "version": "0.1"}, "capabilities": {"experimentalApi": True}})
        if "error" in reply:
            raise RuntimeError(reply)
        await self.connection.send(json.dumps({"method": "initialized"}))


def summarize(response):
    if "error" in response:
        return response
    result = response.get("result", {})
    thread = result.get("thread", {})
    return {key: result.get(key) for key in ("model", "modelProvider", "cwd", "reasoningEffort", "approvalPolicy")} | {
        "thread": {key: thread.get(key) for key in ("id", "ephemeral", "status", "canAcceptDirectInput")}
    }


async def probe(url, token, workspace, helper, fixture):
    output = {}
    try:
        async with connect(url, open_timeout=5):
            output["unauthenticated"] = "UNEXPECTEDLY ACCEPTED"
    except websockets.InvalidStatusCode as error:
        output["unauthenticated"] = {"httpStatus": error.status_code}
    headers = {"Authorization": "Bearer " + token}
    async with connect(url, extra_headers=headers, max_size=48 * 1024 * 1024) as socket_a:
        async with connect(url, extra_headers=headers, max_size=48 * 1024 * 1024) as socket_b:
            a, b = Client(socket_a), Client(socket_b)
            await a.initialize("photocraft-shared-probe-a")
            await b.initialize("photocraft-shared-probe-b")
            def configuration(name):
                return {"features.shell_tool": False, "features.image_generation": False,
                        "features.apps": False, "features.plugins": False,
                        "mcp_servers": {name: {"command": str(helper), "args": ["--ai-canvas-mcp", str(fixture)],
                                                "required": True, "startup_timeout_sec": 15}}}

            created = await a.call("thread/start", {"cwd": str(workspace), "ephemeral": False, "model": "probe-model-a", "modelProvider": "probe",
                                                   "approvalPolicy": "never", "sandbox": "read-only",
                                                   "developerInstructions": "Probe A: no model turn may be started.",
                                                   "config": configuration("probe_canvas_a")})
            output["created"] = summarize(created)
            thread_id = created.get("result", {}).get("thread", {}).get("id")
            if thread_id:
                if created["result"].get("modelProvider") != "probe":
                    raise RuntimeError("Refusing to start any turn: local mock provider was not confirmed")
                MockResponses.hold.set()
                first = await a.call("turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": "Local mock only."}]})
                output["local_mock_turn"] = first
                if "result" not in first:
                    raise RuntimeError(first)
                output["local_mock_completed"] = await a.completed(first["result"]["turn"]["id"])
                output["inventory_before"] = await a.call("mcpServerStatus/list", {"threadId": thread_id, "limit": 100})
                resumed = await b.call("thread/resume", {"threadId": thread_id, "excludeTurns": True,
                                                         "model": "probe-model-b", "modelProvider": "probe", "approvalPolicy": "never", "sandbox": "read-only",
                                                         "developerInstructions": "Probe B: no model turn may be started.",
                                                         "config": configuration("probe_canvas_b")})
                output["resumed_second_connection"] = summarize(resumed)
                output["inventory_after_b"] = await b.call("mcpServerStatus/list", {"threadId": thread_id, "limit": 100})
                output["inventory_after_a"] = await a.call("mcpServerStatus/list", {"threadId": thread_id, "limit": 100})
                output["read_after"] = summarize(await a.call("thread/read", {"threadId": thread_id, "includeTurns": False}))
                output["same_thread"] = resumed.get("result", {}).get("thread", {}).get("id") == thread_id
                MockResponses.hold.clear()
                MockResponses.entered.clear()
                active = await a.call("turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": "Hold local mock response."}]})
                output["active_first_turn"] = active
                for _ in range(100):
                    if MockResponses.entered.is_set():
                        break
                    await asyncio.sleep(0.1)
                output["mock_request_active"] = MockResponses.entered.is_set()
                output["active_second_turn"] = await b.call("turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": "Second local client submission."}]})
                MockResponses.hold.set()
                if "result" in active:
                    output["active_completed"] = await a.completed(active["result"]["turn"]["id"])
            output["notification_methods_a"] = [event.get("method") for event in a.notifications]
            output["notification_methods_b"] = [event.get("method") for event in b.notifications]
            output["mock_requests"] = [{"model": request.get("model"),
                                         "has_instruction_a": "Probe A" in json.dumps(request),
                                         "has_instruction_b": "Probe B" in json.dumps(request),
                                         "tools": [tool.get("name", tool.get("type")) for tool in request.get("tools", [])]} for request in MockResponses.requests]
    print(json.dumps(output, indent=2))


def main():
    options = arguments()
    with tempfile.TemporaryDirectory(prefix="photocraft-shared-probe-") as temporary:
        root = Path(temporary).resolve()
        if root.parent != Path(tempfile.gettempdir()).resolve():
            raise RuntimeError("Unexpected temporary root")
        if os.name == "nt":
            user = subprocess.check_output(["whoami"], text=True).strip()
            subprocess.run(["icacls", str(root), "/inheritance:r", "/grant:r", user + ":(OI)(CI)F"], check=True, capture_output=True)
        home, workspace = root / "home", root / "workspace"
        home.mkdir()
        workspace.mkdir()
        fixture = root / "canvas.json"
        fixture.write_text(json.dumps(FIXTURE), encoding="ascii")
        token = secrets.token_urlsafe(32)
        token_file = root / "token"
        token_file.write_text(token, encoding="ascii")
        with socket.socket() as port_socket:
            port_socket.bind(("127.0.0.1", 0))
            port = port_socket.getsockname()[1]
        url = f"ws://127.0.0.1:{port}"
        env = {key: value for key, value in os.environ.items() if not key.upper().startswith(("CODEX_", "OPENAI_"))}
        env["CODEX_HOME"] = str(home)
        env["HTTP_PROXY"] = env["HTTPS_PROXY"] = "http://127.0.0.1:9"
        env["NO_PROXY"] = "127.0.0.1,localhost"
        env["http_proxy"] = env["https_proxy"] = env["HTTP_PROXY"]
        env["no_proxy"] = env["NO_PROXY"]
        mock = ThreadingHTTPServer(("127.0.0.1", 0), MockResponses)
        mock_thread = threading.Thread(target=mock.serve_forever, daemon=True)
        mock_thread.start()
        args = [str(options.codex), "app-server", "--listen", url, "--ws-auth", "capability-token", "--ws-token-file", str(token_file),
                "-c", 'cli_auth_credentials_store="file"', "-c", 'model_provider="probe"',
                "-c", 'model="probe-model-a"', "-c", 'features.plugins=false', "-c", 'features.apps=false',
                "-c", 'model_providers.probe.name="Local probe"',
                "-c", f'model_providers.probe.base_url="http://127.0.0.1:{mock.server_port}/v1"',
                "-c", 'model_providers.probe.wire_api="responses"', "-c", 'model_providers.probe.requires_openai_auth=false',
                "-c", 'model_providers.probe.supports_websockets=false']
        log_path = root / "server.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen(args, cwd=workspace, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                       creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError("Temporary app-server exited before readiness")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    raise TimeoutError("Temporary app-server startup")
                asyncio.run(probe(url, token, workspace, options.photocraft, fixture))
            finally:
                MockResponses.hold.set()
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                mock.shutdown()
                mock.server_close()
                mock_thread.join(timeout=5)
        print("SERVER_LOG", log_path.read_text(encoding="utf-8", errors="replace").replace(token, "[REDACTED]"))


if __name__ == "__main__":
    main()
