"""Execute the shipped Lua bridge against both editor rotation contracts.

Requires lupa (test-only); run: python scripts/test_defold_rotation_bridge.py
"""
from pathlib import Path
import unittest

from lupa.lua54 import LuaRuntime


SCRIPT = (Path(__file__).resolve().parents[1] / "crates/engine/src/defold_live.lua").read_text(encoding="utf-8")
MOCK = r'''
nodes = {}
writes, mutations, reloads = 0, 0, 0
editor = {version=VERSION, ui={open_resource=function() end}, tx={}}
json = {encode=function() return "{}" end}
print = function() end
editor.properties = function(n) return {"id","type","rotation","position"} end
editor.can_get = function(n,k) return k=="nodes" end
editor.can_set = function() return true end
editor.can_add = function() return true end
editor.get = function(n,k)
    if k=="nodes" then
        local result={}
        for _,v in ipairs(nodes) do if v.parent==n then result[#result+1]=v end end
        return result
    end
    if k=="textures" then return {} end
    return n[k]
end
editor.tx.set = function(n,k,v)
    if k=="rotation" then
        writes=writes+1
        assert(not BROKEN, "pose/rotation assertion")
        assert(#v==3)
    end
    return function() n[k]=v end
end
editor.tx.add = function(parent,k,v)
    return function()
        v.parent=parent
        v.rotation=BROKEN and {0,0,0,1} or {0,0,0}
        nodes[#nodes+1]=v
    end
end
editor.tx.reorder = function() return function() end end
editor.tx.remove = function(parent,k,n)
    return function() for i,v in ipairs(nodes) do if v==n then table.remove(nodes,i);break end end end
end
editor.transact = function(tx)
    mutations=mutations+1
    for _,f in ipairs(tx) do f() end
end
editor.create_directory = function() reloads=reloads+1 end
function setup(rotation, requested, new_node, reparent)
    local root={id="root",type="gui-node-type-box",rotation=rotation,position={0,0,0},parent="/test.gui"}
    if not EMPTY then nodes={root} end
    local writable={"id","type","position"}
    if #rotation==3 then table.insert(writable,3,"rotation") end
    local expected={}
    if not EMPTY then expected={{id="root",parent="",properties={id=root.id,type=root.type,rotation=root.rotation,position=root.position},writable=writable}} end
    REQUEST={op="publishDesign",path="/test.gui",directory="/photocraft_generated/test_123",textures={},expected=expected,nodes={}}
    if not EMPTY then REQUEST.nodes={{id="root",parent="",kind=root.type,values={rotation=requested,position={25,40,0}}}} end
    if new_node then table.insert(REQUEST.nodes,{id="new",parent=EMPTY and "" or "root",kind=root.type,values={rotation=NEW_ROTATION or {0,0,0}}}) end
    if reparent then REQUEST.nodes[1].kind="gui-node-type-text" end
end
'''


class RotationBridgeTests(unittest.TestCase):
    def run_bridge(self, *, broken=True, version="1.14.1-patch.8", rotation=None,
                   requested=None, new=False, rebuild=False, empty=False,
                   new_rotation=None, error=None):
        lua = LuaRuntime()
        g = lua.globals()
        g.BROKEN, g.VERSION, g.EMPTY = broken, version, empty
        if new_rotation is not None:
            g.NEW_ROTATION = lua.table_from(new_rotation)
        lua.execute(MOCK)
        g.setup(lua.table_from(rotation or ([0, 0, 0, 1] if broken else [0, 0, 0])),
                lua.table_from(requested or [0, 0, 0]), new, rebuild)
        if error:
            with self.assertRaisesRegex(Exception, error):
                lua.execute(SCRIPT)
            self.assertEqual(g.mutations, 0)
            self.assertEqual(g.reloads, 0)
        else:
            lua.execute(SCRIPT)
        if broken:
            self.assertEqual(g.writes, 0)
        return g

    def test_moves_and_new_layers_preserve_rotation_on_fork_versions(self):
        for version in ["1.13.1", "1.14.1-patch.8", "2.0.0"]:
            with self.subTest(version=version):
                g = self.run_bridge(version=version, new=True)
                self.assertEqual(g.nodes[1].position[1], 25)
                self.assertEqual(len(g.nodes), 2)

    def test_unchanged_nonzero_rotation_is_not_written(self):
        self.run_bridge(rotation=[0, 0, 2**-0.5, 2**-0.5], requested=[0, 0, 90])

    def test_changed_rotation_stops_before_any_mutation(self):
        self.run_bridge(requested=[0, 0, 45], error="cannot write rotation safely")

    def test_rotated_rebuild_stops_before_any_mutation(self):
        self.run_bridge(rotation=[0, 0, 2**-0.5, 2**-0.5], requested=[0, 0, 90],
                        rebuild=True, error="Rotated hierarchy")

    def test_new_rotation_on_broken_contract_stops_before_mutation(self):
        self.run_bridge(new=True, new_rotation=[0, 0, 30], error="cannot write rotation safely")

    def test_empty_gui_can_add_identity_node_without_rotation_setter(self):
        self.run_bridge(empty=True, new=True)

    def test_corrected_editor_accepts_euler_rotation_regardless_of_version(self):
        for version in ["1.13.1", "1.14.1"]:
            g = self.run_bridge(broken=False, version=version, requested=[0, 0, 45],
                                new=True, new_rotation=[0, 0, 30])
            self.assertEqual(g.writes, 2)

    def test_legacy_add_rejects_rotation_before_creating_node(self):
        lua = LuaRuntime()
        g = lua.globals()
        g.BROKEN, g.VERSION = True, "1.14.1-patch.8"
        lua.execute(MOCK)
        g.setup(lua.table_from([0, 0, 0, 1]), lua.table_from([0, 0, 0]), False, False)
        lua.execute('REQUEST.op="add"; REQUEST.parent="root"; REQUEST.id="new"; REQUEST.kind="gui-node-type-box"; REQUEST.values={rotation={0,0,45}}')
        with self.assertRaisesRegex(Exception, "cannot write rotation safely"):
            lua.execute(SCRIPT)
        self.assertEqual(g.mutations, 0)

    def test_layer_rename_reuses_the_existing_editor_node(self):
        for broken in [False, True]:
            lua = LuaRuntime()
            g = lua.globals()
            g.BROKEN, g.VERSION = broken, "1.14.1-patch.15"
            lua.execute(MOCK)
            g.setup(lua.table_from([0,0,0,1] if broken else [0,0,0]), lua.table_from([0,0,0]), True, False)
            lua.execute('old=nodes[1];REQUEST.renames={{from="root",to="Reward"}};REQUEST.nodes[1].id="Reward";REQUEST.nodes[2].parent="Reward"')
            lua.execute(SCRIPT)
            lua.execute('assert(nodes[1]==old and old.id=="Reward" and #nodes==2 and nodes[2].parent==old)')

    def test_invalid_angles_stop_before_any_mutation(self):
        for value in [float("nan"), float("inf")]:
            self.run_bridge(requested=[0, 0, value], error="Invalid rotation")


if __name__ == "__main__":
    unittest.main()
