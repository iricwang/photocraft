"""Offline evidence checks only: no account, subprocess or model calls."""
import base64
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

from verify_existing_canvas import audit_rollout


class RolloutAuditTests(unittest.TestCase):
    def audit(self, lookup, *, correct_pixels=True, completed=True, image_calls=1):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            def write(name, value):
                (out / name).write_text(json.dumps(value), encoding='utf-8')
            write('submitted.json', {'thread':'original', 'turn':'one'})
            write('receipt.json', {'thread':'original', 'turn':'one', 'completed':True,
                                  'imageSaved':True, 'canvasRead':False, 'imageStartedAfterCanvas':False})
            write('events.json', [{'event':'item/completed', 'type':'mcpToolCall',
                                  'server':'photocraft_canvas', 'tool':'canvas_snapshot',
                                  'status':'completed' if completed else 'failed'}])
            source = b'opaque image fixture for byte-identity comparison'
            (out / 'source-canvas.png').write_bytes(source)
            encoded = base64.b64encode(source if correct_pixels else b'different image').decode()
            entries = [
                {'type':'turn_context', 'payload':{'turn_id':'one'}},
                {'type':'response_item', 'timestamp':'01', 'payload':{
                    'type':'custom_tool_call', 'call_id':'canvas', 'input':lookup}},
                {'type':'response_item', 'timestamp':'02', 'payload':{
                    'type':'custom_tool_call_output', 'call_id':'canvas', 'output':[
                        {'type':'input_image', 'image_url':'data:image/png;base64,' + encoded}]}},
            ]
            for index in range(image_calls):
                entries.append({'type':'response_item', 'timestamp':str(3 + index), 'payload':{
                    'type':'custom_tool_call', 'call_id':f'image-{index}',
                    'input':'await tools.image_gen__imagegen({num_last_images_to_include:2})'}})
            rollout = out / 'rollout.jsonl'
            rollout.write_text('\n'.join(json.dumps(e) for e in entries), encoding='utf-8')
            with contextlib.redirect_stdout(io.StringIO()):
                audit_rollout(out, rollout)
            self.assertTrue(json.loads((out / 'receipt.json').read_text())['canvasRead'])

    def test_direct_lookup(self):
        self.audit('await tools.mcp__photocraft_canvas__canvas_snapshot({})')

    def test_dynamic_lookup(self):
        self.audit('const t = ALL_TOOLS.find(x => /canvas_snapshot/.test(x.name)); await tools[t.name]({})')

    def test_different_pixels_are_not_accepted(self):
        with self.assertRaises(AssertionError):
            self.audit('await tools[t.name]({})', correct_pixels=False)

    def test_failed_canvas_tool_is_not_accepted(self):
        with self.assertRaises(AssertionError):
            self.audit('await tools[t.name]({})', completed=False)

    def test_repeated_generation_is_not_accepted(self):
        with self.assertRaises(AssertionError):
            self.audit('await tools[t.name]({})', image_calls=2)


if __name__ == '__main__':
    unittest.main()
