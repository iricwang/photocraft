"""Opt-in, one-turn acceptance against an explicitly approved existing design conversation."""
import argparse
import base64
import json
import hashlib
from pathlib import Path
import queue
import re
import subprocess
import time

from probe_codex_bridge import Peer


def audit_rollout(out, rollout):
    """Audit model-visible items; MCP summary notifications may omit image blocks."""
    marker = json.loads((out / 'submitted.json').read_text(encoding='utf-8'))
    receipt = json.loads((out / 'receipt.json').read_text(encoding='utf-8'))
    assert receipt['thread'] == marker['thread'] and receipt['turn'] == marker['turn']
    events = json.loads((out / 'events.json').read_text(encoding='utf-8'))
    canvas_completed = any(e.get('event') == 'item/completed' and e.get('type') == 'mcpToolCall'
                           and e.get('server') == 'photocraft_canvas' and e.get('tool') == 'canvas_snapshot'
                           and e.get('status') == 'completed' and not e.get('error') for e in events)
    source_hash = hashlib.sha256((out / 'source-canvas.png').read_bytes()).hexdigest()
    canvas_at = None
    image_at = None
    image_calls = 0
    includes_two = False
    active = False
    canvas_hash = None
    with rollout.open(encoding='utf-8') as stream:
        for line in stream:
            event = json.loads(line)
            payload = event.get('payload', {})
            if event.get('type') == 'turn_context':
                active = payload.get('turn_id') == marker['turn']
            if not active or event.get('type') != 'response_item':
                continue
            typ = payload.get('type')
            code = payload.get('input', '')
            if typ == 'custom_tool_call' and isinstance(code, str):
                if re.search(r'await\s+tools\.image_gen__imagegen\s*\(', code):
                    image_calls += 1
                    image_at = event['timestamp']
                    includes_two = bool(re.search(r'num_last_images_to_include["\']?\s*:\s*2\b', code))
                    assert canvas_at is not None, 'Image generation started before the canvas image reached the model'
            # Dynamic tool lookup can use tools[t.name]; identify returned pixels by
            # exact source hash instead of trying to parse arbitrary JavaScript.
            if typ == 'custom_tool_call_output' and canvas_at is None:
                for block in payload.get('output', []):
                    if block.get('type') == 'input_image':
                        url = block['image_url']
                        if not url.startswith('data:image/png;base64,'):
                            continue
                        png = base64.b64decode(url.split(',', 1)[1], validate=True)
                        if hashlib.sha256(png).hexdigest() != source_hash:
                            continue
                        (out / 'canvas-returned.png').write_bytes(png)
                        canvas_hash = hashlib.sha256(png).hexdigest()
                        canvas_at = event['timestamp']
    evidence = {'thread':marker['thread'], 'turn':marker['turn'], 'canvasReceivedAt':canvas_at,
                'imageRequestedAt':image_at, 'imageCalls':image_calls, 'includesTwoImages':includes_two,
                'canvasSha256':canvas_hash,
                'sourceCanvasSha256':source_hash, 'canvasToolCompleted':canvas_completed}
    (out / 'model-evidence.json').write_text(json.dumps(evidence, indent=2), encoding='utf-8')
    assert canvas_completed and canvas_at and image_at and canvas_at < image_at and image_calls == 1 and includes_two, evidence
    assert evidence['canvasSha256'] == evidence['sourceCanvasSha256'], 'Tool returned a different canvas snapshot'
    assert receipt['completed'] and receipt['imageSaved'], 'Turn did not complete with an image'
    receipt.setdefault('summaryTelemetry', {k:receipt[k] for k in ('canvasRead', 'imageStartedAfterCanvas')})
    receipt.update(canvasRead=True, imageStartedAfterCanvas=True, verifiedFrom='model-visible rollout')
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2), encoding='utf-8')
    print(json.dumps(evidence, indent=2), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge')
    parser.add_argument('--bundle', type=Path)
    parser.add_argument('--thread')
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--approve-one-real-turn', action='store_true')
    parser.add_argument('--audit-only', action='store_true')
    parser.add_argument('--rollout', type=Path)
    args = parser.parse_args()
    if args.audit_only:
        if not args.rollout:
            parser.error('--audit-only requires --rollout')
        audit_rollout(args.out, args.rollout)
        return
    if not all((args.bridge, args.bundle, args.thread, args.approve_one_real_turn)):
        parser.error('A real turn requires --bridge, --bundle, --thread and --approve-one-real-turn')
    bundle = json.loads(args.bundle.read_text(encoding='utf-8'))
    args.out.mkdir(parents=True, exist_ok=True)
    marker = args.out / 'submitted.json'
    if marker.exists():
        raise RuntimeError('This acceptance already submitted a turn; inspect its receipt, never retry automatically')
    p = subprocess.Popen([args.bridge, '--photocraft-client'], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                         encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
    peer = Peer(p)
    events = []
    turn_id = None
    def record(value):
        events.append(value)
        print(json.dumps(value, ensure_ascii=True), flush=True)
        (args.out / 'events.json').write_text(json.dumps(events, ensure_ascii=False, indent=2), encoding='utf-8')
    try:
        peer.initialize('photocraft-approved-canvas-acceptance')
        read = peer.call('thread/read', {'threadId':args.thread, 'includeTurns':False})
        thread = read.get('result', {}).get('thread', {})
        assert thread.get('id') == args.thread and thread.get('status', {}).get('type') == 'idle', read.get('error', 'Thread is not idle')
        resumed = peer.call('thread/resume', dict(bundle['threadParams'], threadId=args.thread, excludeTurns=True))
        assert resumed.get('result', {}).get('thread', {}).get('id') == args.thread, resumed.get('error')
        inventory = peer.call('mcpServerStatus/list', {'threadId':args.thread, 'serverName':'photocraft_canvas', 'limit':1})
        server = next(s for s in inventory['result']['data'] if s['name'] == 'photocraft_canvas')
        assert server.get('runtimeStatus') == 'connected' and 'canvas_snapshot' in server['tools'], 'Canvas tool unavailable'
        injected = peer.call('thread/inject_items', {'threadId':args.thread, 'items':[
            {'type':'message', 'role':'developer', 'content':[{'type':'input_text','text':bundle['instructions']}]}]})
        assert 'result' in injected, injected.get('error')
        record({'stage':'instructions_confirmed','thread':args.thread,'canvasTool':'connected'})
        params = dict(bundle['turnParams'], threadId=args.thread)
        # Reserve before the network write: uncertain delivery must never permit a repeat.
        with marker.open('x', encoding='utf-8') as f:
            json.dump({'thread':args.thread,'submittedAt':time.time(),'status':'submitting'}, f)
        response = peer.call('turn/start', params)
        assert 'result' in response, response.get('error')
        turn_id = response['result']['turn']['id']
        marker.write_text(json.dumps({'thread':args.thread,'turn':turn_id,'status':'submitted'}), encoding='utf-8')
        record({'stage':'turn_started','thread':args.thread,'turn':turn_id})
        end = time.monotonic() + 600
        canvas_read = False
        image_started_after_canvas = False
        image_saved = False
        completed = False
        while time.monotonic() < end:
            try:
                event = peer.pending.pop(0) if peer.pending else peer.events.get(timeout=1)
            except queue.Empty:
                continue
            if 'closed' in event:
                raise RuntimeError('Connection closed; inspect original turn, do not retry')
            ep = event.get('params', {})
            if ep.get('threadId') != args.thread:
                continue
            if event.get('id') is not None and event.get('method'):
                peer.send({'id':event['id'],'error':{'code':-32601,'message':'Acceptance permits only configured tools'}})
                continue
            method = event.get('method')
            if ep.get('turnId', ep.get('turn', {}).get('id')) != turn_id:
                continue
            item = ep.get('item', {})
            typ = item.get('type')
            if method in ('item/started','item/completed'):
                entry = {'event':method,'type':typ}
                if typ == 'mcpToolCall':
                    result = item.get('result') or {}
                    blocks = result.get('content', [])
                    entry.update({k:item.get(k) for k in ('server','tool','status','error')})
                    entry['contentTypes'] = [b.get('type') for b in blocks]
                    if method == 'item/completed' and item.get('tool') == 'canvas_snapshot':
                        canvas_read = item.get('status') == 'completed' and any(b.get('type') == 'image' for b in blocks) and not result.get('isError')
                        for b in blocks:
                            if b.get('type') == 'image':
                                (args.out / 'canvas-returned.png').write_bytes(base64.b64decode(b['data'], validate=True))
                elif typ == 'imageGeneration':
                    if method == 'item/started':
                        image_started_after_canvas = canvas_read
                    if method == 'item/completed' and item.get('result'):
                        encoded = item['result'].removeprefix('data:image/png;base64,')
                        (args.out / 'generated.png').write_bytes(base64.b64decode(encoded, validate=True))
                        image_saved = True
                    entry.update(status=item.get('status'), failure=item.get('failure'), canvasReadBeforeImage=image_started_after_canvas)
                elif typ == 'agentMessage':
                    entry['text'] = item.get('text','')[:1600]
                record(entry)
            if method == 'turn/completed':
                completed = ep['turn'].get('status') == 'completed'
                break
        receipt = {'thread':args.thread,'turn':turn_id,'canvasRead':canvas_read,
                   'imageStartedAfterCanvas':image_started_after_canvas,'imageSaved':image_saved,'completed':completed}
        (args.out / 'receipt.json').write_text(json.dumps(receipt, indent=2), encoding='utf-8')
        record(receipt)
        rollout = args.rollout or (Path(thread['path']) if thread.get('path') else None)
        if rollout:
            audit_rollout(args.out, rollout)
        elif not all((canvas_read, image_started_after_canvas, image_saved, completed)):
            raise RuntimeError('Real acceptance failed; results retained, no retry performed')
    finally:
        peer.close()
        if p.returncode:
            print('Shared client error: ' + p.stderr.read()[:2000], flush=True)


if __name__ == '__main__':
    main()
