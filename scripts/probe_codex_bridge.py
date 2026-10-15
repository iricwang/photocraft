"""Exercise the bundled bridge against real Codex and loopback mock inference only."""
import argparse
from http.server import ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time

from probe_shared_codex import MockResponses, FIXTURE

TURN_POLICY = "Current PhotoCraft workflow: read photocraft_canvas.canvas_snapshot before editing existing artwork; skip for unrelated reference-only assets. Keep the user's request unchanged."


def developer_text(request):
    return "\n".join(part.get("text", "") for item in request["input"] if item.get("role") == "developer" for part in item.get("content", []))


class CanvasResponses(MockResponses):
    def output_item(self, request):
        if len(self.requests) == 2:
            assert TURN_POLICY in developer_text(request), "Current policy never reached model-visible developer instructions"
            namespace = next(t for t in request["tools"] if t.get("name") == "mcp__photocraft_canvas")
            assert any(t.get("name") == "canvas_snapshot" for t in namespace["tools"])
            assert "before generating or asking the user for a screenshot" in namespace["description"]
            assert "Do not call it for an unrelated new image" in namespace["description"]
            return {"id": "fc_canvas_probe", "type": "function_call", "status": "completed",
                    "namespace": "mcp__photocraft_canvas", "name": "canvas_snapshot",
                    "call_id": "call_canvas_probe", "arguments": "{}"}
        return super().output_item(request)


def has_canvas_image(value):
    if isinstance(value, dict):
        if value.get("type") == "input_image" and str(value.get("image_url", "")).startswith("data:image/"):
            return True
        return any(has_canvas_image(v) for v in value.values())
    return isinstance(value, list) and any(has_canvas_image(v) for v in value)


class Peer:
    def __init__(self, process):
        self.process = process
        self.events = queue.Queue()
        self.serial = 0
        self.pending = []
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for line in self.process.stdout:
            try:
                self.events.put(json.loads(line))
            except ValueError:
                self.events.put({"invalid": line})
        self.events.put({"closed": True})

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def until(self, predicate):
        for i, event in enumerate(self.pending):
            if predicate(event):
                return self.pending.pop(i)
        end = time.monotonic() + 45
        while time.monotonic() < end:
            event = self.events.get(timeout=max(0.01, end - time.monotonic()))
            if "closed" in event or "invalid" in event:
                raise RuntimeError(event)
            if predicate(event):
                return event
            self.pending.append(event)
        raise TimeoutError("Bridge reply")

    def call(self, method, params):
        self.serial += 1
        serial = self.serial
        self.send({"id": serial, "method": method, "params": params})
        return self.until(lambda e: e.get("id") == serial and "method" not in e)

    def initialize(self, name):
        reply = self.call("initialize", {"clientInfo": {"name": name, "version": "0.1"}, "capabilities": {"experimentalApi": True}})
        assert "result" in reply, reply
        self.send({"method": "initialized"})

    def complete(self, turn):
        reply = self.until(lambda e: e.get("method") == "turn/completed" and e["params"]["turn"]["id"] == turn)
        assert reply["params"]["turn"]["status"] == "completed", reply

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise RuntimeError("Owned bridge did not shut down after EOF")
        self.reader.join(timeout=3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", required=True, type=Path)
    parser.add_argument("--codex", required=True, type=Path)
    args = parser.parse_args()
    bridge, codex = args.bridge.resolve(strict=True), args.codex.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="photocraft-bridge-probe-") as temporary:
        root = Path(temporary).resolve()
        home, workspace, config = root / "home", root / "workspace", root / "bridge"
        for path in (home, workspace, config):
            path.mkdir()
        (config / "config.json").write_text(json.dumps({"version": 1, "enabled": True, "realCodex": str(codex), "executable": str(bridge)}))
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "PHOTOCRAFT_"))}
        env.update(CODEX_HOME=str(home), PHOTOCRAFT_CODEX_BRIDGE_DIR=str(config), HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        env.update(http_proxy=env["HTTP_PROXY"], https_proxy=env["HTTPS_PROXY"], no_proxy=env["NO_PROXY"])
        mock = ThreadingHTTPServer(("127.0.0.1", 0), CanvasResponses)
        mock_thread = threading.Thread(target=mock.serve_forever, daemon=True)
        mock_thread.start()
        command = [str(bridge), "app-server", "--listen", "stdio://"]
        for setting in ['cli_auth_credentials_store="file"', 'model_provider="probe"', 'model="probe-model-a"',
                        'features.plugins=false', 'features.apps=false', 'model_providers.probe.name="Local probe"',
                        f'model_providers.probe.base_url="http://127.0.0.1:{mock.server_port}/v1"',
                        'model_providers.probe.wire_api="responses"', 'model_providers.probe.requires_openai_auth=false',
                        'model_providers.probe.supports_websockets=false']:
            command.extend(["-c", setting])
        peers = []
        with (root / "bridge.log").open("w", encoding="utf-8") as log:
            def launch(argv):
                process = subprocess.Popen(argv, cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                           text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
                peer = Peer(process)
                peers.append(peer)
                return peer
            try:
                desktop = launch(command)
                desktop.initialize("photocraft-probe-desktop")
                created = desktop.call("thread/start", {"cwd": str(workspace), "ephemeral": False, "modelProvider": "probe", "model": "probe-model-a",
                                                       "developerInstructions": "Legacy workflow: images arrive as user attachments.",
                                                       "approvalPolicy": "never", "sandbox": "read-only", "config": {"features.shell_tool": False, "mcp_servers": {}}})
                assert created.get("result", {}).get("modelProvider") == "probe", created
                thread = created["result"]["thread"]["id"]
                MockResponses.hold.set()
                turn = desktop.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "Local mock: initial persistent history."}]})
                desktop.complete(turn["result"]["turn"]["id"])
                desktop.close()
                assert not (config / "endpoint.json").exists(), "Endpoint survived owner EOF"
                lease = workspace / ".photocraft-canvas-active.json"
                lease.write_text(json.dumps({"version": 1, "leaseId": "probe", "expiresAtMs": int(time.time() * 1000) + 600000, "snapshot": FIXTURE}))
                params = {"config": {"features.shell_tool": False, "mcp_servers": {"photocraft_canvas": {
                    "command": str(bridge), "args": ["--canvas-mcp-live", str(lease)], "required": True, "startup_timeout_sec": 20}}}}
                (config / "templates.json").write_text(json.dumps({thread: {"workspace": str(workspace), "params": params}}))
                desktop = launch(command)
                desktop.initialize("photocraft-probe-desktop")
                resumed = desktop.call("thread/resume", {"threadId": thread})
                assert resumed.get("result", {}).get("thread", {}).get("id") == thread, resumed
                assert resumed["result"]["modelProvider"] == "probe", resumed
                inventory = desktop.call("mcpServerStatus/list", {"threadId": thread, "limit": 100})
                assert "canvas_snapshot" in json.dumps(inventory), inventory
                assert "project_context" in json.dumps(inventory), inventory
                descriptor = json.loads((config / "endpoint.json").read_text())
                host, port = descriptor["address"].rsplit(":", 1)
                with socket.create_connection((host, int(port)), timeout=5) as sock:
                    sock.sendall(b'{"version":1,"token":"wrong"}\n')
                    rejected = sock.recv(4096)
                    assert not rejected or b'"error"' in rejected or b'false' in rejected, rejected
                photo = launch([str(bridge), "--photocraft-client"])
                photo.initialize("photocraft")
                second = photo.call("thread/resume", {"threadId": thread})
                assert second.get("result", {}).get("thread", {}).get("id") == thread, second
                MockResponses.hold.clear()
                MockResponses.entered.clear()
                injected = photo.call("thread/inject_items", {"threadId": thread, "items": [
                    {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": TURN_POLICY}]}]})
                assert "result" in injected, injected
                active = photo.call("turn/start", {"threadId": thread, "model": "probe-model-b",
                    "collaborationMode": {"mode": "default", "settings": {"model": "probe-model-b", "reasoning_effort": "high", "developer_instructions": TURN_POLICY}},
                    "input": [{"type": "text", "text": "Local mock: next design."}]})
                assert "result" in active, active
                assert MockResponses.entered.wait(15), "Mock provider did not receive turn"
                busy = desktop.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "Must not steer the active design."}]})
                assert "error" in busy, busy
                # No events for longer than either socket authentication timeout.
                time.sleep(8)
                assert photo.process.poll() is None, "Client disconnected while waiting for the model"
                MockResponses.hold.set()
                photo.complete(active["result"]["turn"]["id"])
                desktop.call("thread/read", {"threadId": thread, "includeTurns": True})
                updated_policy = TURN_POLICY + " Latest workflow revision: keep source and reference roles separate."
                injected = photo.call("thread/inject_items", {"threadId": thread, "items": [
                    {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": updated_policy}]}]})
                assert "result" in injected, injected
                again = photo.call("turn/start", {"threadId": thread, "model": "probe-model-c",
                    "collaborationMode": {"mode": "default", "settings": {"model": "probe-model-c", "reasoning_effort": "high", "developer_instructions": updated_policy}},
                    "input": [{"type": "text", "text": "Local mock: continue same design."}]})
                assert "result" in again, again
                photo.complete(again["result"]["turn"]["id"])
                assert len(MockResponses.requests) == 4, len(MockResponses.requests)
                assert not has_canvas_image(MockResponses.requests[1]["input"]), "Canvas was attached before the tool call"
                assert has_canvas_image(MockResponses.requests[2]["input"]), "Tool image did not reach the model"
                assert TURN_POLICY in developer_text(MockResponses.requests[2]), "Policy missing after tool result"
                assert MockResponses.requests[1].get("reasoning", {}).get("effort") == "high"
                assert MockResponses.requests[-1]["model"] == "probe-model-c"
                assert updated_policy in developer_text(MockResponses.requests[-1]), "Updated rules missing on unchanged default mode"
                photo.close()
                desktop.close()
                assert not (config / "endpoint.json").exists()
                print(json.dumps({"sameThread": thread, "mockTurns": 3, "modelRequests": 4, "canvasToolImageRoundtrip": "passed", "desktopFirstResume": "passed", "liveCanvasTool": "passed",
                                  "perTurnDeveloperInstructions": "passed", "concurrentStartRejected": "passed", "idleWaitSeconds": 8, "perTurnModel": "passed", "authentication": "passed", "eofShutdown": "passed"}, indent=2))
            except Exception:
                log.flush()
                print((root / "bridge.log").read_text(encoding="utf-8", errors="replace"), flush=True)
                raise
            finally:
                MockResponses.hold.set()
                for peer in reversed(peers):
                    peer.close()
                mock.shutdown()
                mock.server_close()
                mock_thread.join(timeout=5)
        print("All inference stayed on the loopback mock; no account credentials or user conversations were used.")


if __name__ == "__main__":
    main()
