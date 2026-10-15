-- Executed only against the user's bound or authenticated local Defold editor.
-- No file writes: GUI mutations use the editor's own atomic graph transaction.
local req = REQUEST
-- Keep the linked resource open in the Defold workspace before reading or editing it.
editor.ui.open_resource(req.path)
local refs, parents = {}, {}
local function plain(v, depth)
    if depth > 12 then error("Property nesting limit") end
    local t = type(v)
    if t == "string" or t == "boolean" or t == "number" or t == "nil" then return v end
    if t ~= "table" then return nil end
    local r = {}
    for k, x in pairs(v) do r[k] = plain(x, depth + 1) end
    return r
end
local function snapshot()
    refs, parents = {}, {}
    local nodes = {}
    local function visit(parent, parent_id, depth)
        if depth > 128 then error("GUI nesting limit") end
        if not editor.can_get(parent, "nodes") then return end
        for _, n in ipairs(editor.get(parent, "nodes")) do
            local id = editor.get(n, "id")
            if not refs[id] then
                if #nodes >= 8192 then error("GUI node limit") end
                refs[id], parents[id] = n, parent
                local props, writable = {}, {}
                for _, k in ipairs(editor.properties(n)) do
                    if k ~= "nodes" then
                        local ok, v = pcall(editor.get, n, k)
                        if ok then props[k] = plain(v, 0) end
                        -- 1.13 returns raw quaternion rotations but its setter truncates
                        -- them to three components. Never feed those values back.
                        local legacy_rotation = k == "rotation" and (string.match(editor.version,"^1%.13%.") or (type(v)=="table" and #v==4))
                        if editor.can_set(n, k) and not legacy_rotation then writable[#writable + 1] = k end
                    end
                end
                nodes[#nodes + 1] = {id=id, parent=parent_id, properties=props, writable=writable}
                visit(n, id, depth + 1)
            end
        end
    end
    visit(req.path, "", 0)
    return {version=editor.version, path=req.path, nodes=nodes}
end
local function same(a,b)
    if type(a)~=type(b) then return false end
    if type(a)~="table" then return a==b end
    for k,v in pairs(a) do if not same(v,b[k]) then return false end end
    for k,_ in pairs(b) do if a[k]==nil then return false end end
    return true
end
local current = snapshot()
if req.op == "publishDesign" then
    assert(same(current.nodes, req.expected), "GUI changed in Defold; nothing was overwritten")
    assert(type(req.directory)=="string" and string.match(req.directory,"^/photocraft_generated/[%w_/]+$"), "Invalid generated directory")
    local wanted, ordered = {}, req.nodes
    assert(type(ordered)=="table" and #ordered<=8192,"Node limit")
    for _,n in ipairs(ordered) do assert(not wanted[n.id],"Duplicate node ID"); wanted[n.id]=n end
    local function degrees(q)
        if #q~=4 then return q end
        local x,y,z,w=q[1],q[2],q[3],q[4]
        return {math.deg(math.atan(2*(w*x+y*z),1-2*(x*x+y*y))),math.deg(math.asin(math.max(-1,math.min(1,2*(w*y-z*x))))),math.deg(math.atan(2*(w*z+x*y),1-2*(y*y+z*z)))}
    end
    local old={}
    for _,n in ipairs(current.nodes) do old[n.id]=n end
    local rebuild={}
    for _,n in ipairs(ordered) do
        local o=old[n.id]
        if o and (o.parent~=n.parent or o.properties.type~=n.kind) then rebuild[n.id]=true end
        local r=n.values.rotation
        if r and string.match(editor.version,"^1%.13%.") then
            local before=o and degrees(o.properties.rotation or {0,0,0}) or {0,0,0}
            for i=1,3 do assert(math.abs((r[i] or 0)-(before[i] or 0))<0.0001,"This Defold version cannot write rotation safely. Close the editor and sync to disk.") end
            n.values.rotation=nil
        end
    end
    for _=1,128 do
        local changed=false
        for _,n in ipairs(current.nodes) do if rebuild[n.parent] and not rebuild[n.id] then rebuild[n.id]=true;changed=true end end
        if not changed then break end
    end
    for id,_ in pairs(rebuild) do
        local r=old[id] and old[id].properties.rotation
        if r and #r==4 and string.match(editor.version,"^1%.13%.") then assert(math.abs(r[1])+math.abs(r[2])+math.abs(r[3])<0.000001,"Rotated hierarchy needs offline sync in this Defold version") end
    end
    -- Public resource creation reloads disk resources, including newly staged atlas/PNG files.
    editor.create_directory(req.directory)
    current=snapshot()
    assert(same(current.nodes,req.expected),"GUI changed during asset reload; publication stopped")
    local resource_tx={}
    for _,texture in ipairs(req.textures) do
        local existing
        for _,t in ipairs(editor.get(req.path,"textures")) do if editor.get(t,"name")==texture.name then existing=t end end
        if existing then resource_tx[#resource_tx+1]=editor.tx.set(existing,"texture",texture.texture)
        else resource_tx[#resource_tx+1]=editor.tx.add(req.path,"textures",texture) end
    end
    if #resource_tx>0 then editor.transact(resource_tx) end
    local removals={}
    for _,n in ipairs(current.nodes) do
        if (not wanted[n.id] or rebuild[n.id]) and (n.parent=="" or (wanted[n.parent] and not rebuild[n.parent])) then
            removals[#removals+1]=editor.tx.remove(parents[n.id],"nodes",refs[n.id])
        end
    end
    if #removals>0 then editor.transact(removals);current=snapshot() end
    -- Attach ancestors before descendants, then assign resource-dependent fields.
    for _=1,128 do
        local tx={}
        for _,n in ipairs(ordered) do
            if not refs[n.id] and (n.parent=="" or refs[n.parent]) then
                tx[#tx+1]=editor.tx.add(n.parent=="" and req.path or refs[n.parent],"nodes",{id=n.id,type=n.kind})
            end
        end
        if #tx==0 then break end
        editor.transact(tx);current=snapshot()
    end
    local updates={}
    for _,n in ipairs(ordered) do
        local ref=assert(refs[n.id],"Could not attach GUI node")
        for k,v in pairs(n.values) do
            if editor.can_set(ref,k) then
                local ok,before=pcall(editor.get,ref,k)
                if not ok or not same(before,v) then updates[#updates+1]=editor.tx.set(ref,k,v) end
            end
        end
    end
    if #updates>0 then editor.transact(updates) end
    current=snapshot()
    local order,owners={},{}
    for _,n in ipairs(ordered) do
        if not order[n.parent] then order[n.parent]={};owners[#owners+1]=n.parent end
        order[n.parent][#order[n.parent]+1]=refs[n.id]
    end
    local reorder={}
    for _,parent in ipairs(owners) do reorder[#reorder+1]=editor.tx.reorder(parent=="" and req.path or refs[parent],"nodes",order[parent]) end
    if #reorder>0 then editor.transact(reorder) end
    print("PHOTOCRAFT_JSON:"..json.encode(snapshot()))
    return
end
if req.op == "openResource" then
    assert(type(req.resource)=="string" and string.sub(req.resource,1,1)=="/","Invalid project resource")
    editor.ui.open_resource(req.resource)
    print("PHOTOCRAFT_JSON:"..json.encode(current))
    return
end
-- Resource choice lists are populated only after a new attachment is connected
-- to its scene. Defold validates e.g. font too early during tx.add, so apply
-- properties after creating the complete hierarchy. Failure is reported and the
-- caller pauses for reconciliation; never retry a partially applied operation.
local deferred = {}
local function skeleton(value)
    local result = {id=value.id, type=value.type}
    local props = {}
    for k,v in pairs(value) do
        if k ~= "nodes" and k ~= "id" and k ~= "type" then props[k]=v end
    end
    deferred[#deferred+1]={id=value.id, properties=props}
    if value.nodes then
        result.nodes={}
        for _,child in ipairs(value.nodes) do result.nodes[#result.nodes+1]=skeleton(child) end
    end
    return result
end
local function attachment(id, depth)
    assert(depth < 128, "GUI depth limit")
    local node
    for _,n in ipairs(current.nodes) do if n.id==id then node=n;break end end
    assert(node, "Missing node")
    assert(node.properties.type ~= "gui-node-type-template", "Template structure must be edited in Defold")
    local rotation=node.properties.rotation
    if type(rotation)=="table" and #rotation==4 then
        assert(math.abs(rotation[1])+math.abs(rotation[2])+math.abs(rotation[3])<0.0000001,
            "This editor cannot safely restore rotated nodes. Use Defold for delete/reparent.")
    end
    local value={type=node.properties.type}
    for _,k in ipairs(node.writable) do value[k]=node.properties[k] end
    local children={}
    for _,n in ipairs(current.nodes) do if n.parent==id then children[#children+1]=attachment(n.id,depth+1) end end
    if #children>0 then value.nodes=children end
    return value
end
if req.op ~= "snapshot" then
    -- Snapshot compare guards stale views, including external edits/deletion/undo.
    -- The Lua API has no compare-and-swap primitive: the subsequent transaction is
    -- atomic, but competing UI edits between preflight and commit remain possible.
    if not same(current.nodes, req.expected) then error("GUI changed in Defold. Refresh before retrying; no edit was applied.") end
    if req.op == "saveProject" then
        assert(req.allModifiedResources==true,"Explicit whole-project save required")
        editor.save()
        print("PHOTOCRAFT_JSON:"..json.encode(snapshot()))
        return
    end
    local tx = {}
    if req.op == "set" then
        local n = assert(refs[req.node], "Node no longer exists")
        for k,v in pairs(req.values) do
            assert(editor.can_set(n,k), "Read-only property: "..k)
            if k=="rotation" then
                assert(not string.match(editor.version,"^1%.13%."),"Rotation writes are incompatible with Defold 1.13; edit rotation in Defold")
                assert(#editor.get(n,k)==3,"Editor rotation setter is incompatible")
            end
            tx[#tx+1] = editor.tx.set(n,k,v)
        end
    elseif req.op == "add" then
        local parent = req.parent ~= "" and assert(refs[req.parent], "Parent no longer exists") or req.path
        assert(not refs[req.id], "Node ID already exists")
        assert(editor.can_add(parent,"nodes"), "Read-only parent")
        local node = req.values or {}
        node.type, node.id = req.kind, req.id
        tx[1] = editor.tx.add(parent,"nodes",skeleton(node))
    elseif req.op == "delete" then
        if editor.can_get(req.path,"layouts") then assert(#editor.get(req.path,"layouts")==0,"Delete with layout overrides must use Defold's native history") end
        attachment(req.node,0)
        local n = assert(refs[req.node], "Node no longer exists")
        tx[1] = editor.tx.remove(parents[req.node],"nodes",n)
    elseif req.op == "move" then
        if editor.can_get(req.path,"layouts") then assert(#editor.get(req.path,"layouts")==0,"Reparent with layout overrides must use Defold") end
        local n=assert(refs[req.node],"Missing node")
        local parent=req.parent~="" and assert(refs[req.parent],"Missing parent") or req.path
        local at=req.parent
        while at~="" do
            assert(at~=req.node,"Cyclic hierarchy")
            local found=false
            for _,node in ipairs(current.nodes) do if node.id==at then at=node.parent;found=true;break end end
            assert(found,"Invalid parent")
        end
        assert(editor.can_add(parent,"nodes"),"Read-only parent")
        tx[1]=editor.tx.remove(parents[req.node],"nodes",n)
        tx[2]=editor.tx.add(parent,"nodes",skeleton(attachment(req.node,0)))
    elseif req.op == "reorder" then
        local parent = req.parent ~= "" and assert(refs[req.parent], "Parent no longer exists") or req.path
        local children = {}
        for _,id in ipairs(req.order) do children[#children+1] = assert(refs[id],"Missing node") end
        tx[1] = editor.tx.reorder(parent,"nodes",children)
    else error("Unsupported bridge operation") end
    assert(#tx>0,"Empty transaction")
    editor.transact(tx)
    current = snapshot()
    if #deferred > 0 then
        local values={}
        for _,item in ipairs(deferred) do
            local node=assert(refs[item.id],"New node ID changed; refresh to reconcile")
            for k,v in pairs(item.properties) do values[#values+1]=editor.tx.set(node,k,v) end
        end
        if #values>0 then editor.transact(values) end
        current=snapshot()
    end
    -- The extension API can only reorder existing node handles, so positioning
    -- a newly created attachment follows its creation transaction.
    if req.after or req.before then
        local id=req.id or req.node
        local parent=req.parent~="" and refs[req.parent] or req.path
        local order={}
        for _,n in ipairs(editor.get(parent,"nodes")) do
            local nid=editor.get(n,"id")
            if nid~=id then
                if nid==req.after then order[#order+1]=assert(refs[id],"New node ID changed") end
                order[#order+1]=n
                if nid==req.before then order[#order+1]=assert(refs[id],"New node ID changed") end
            end
        end
        editor.transact({editor.tx.reorder(parent,"nodes",order)})
        current=snapshot()
    end
end
print("PHOTOCRAFT_JSON:"..json.encode(current))
