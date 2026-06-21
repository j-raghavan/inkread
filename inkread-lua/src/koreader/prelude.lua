-- inkread KOReader compatibility prelude (RR14 / ADR-INKREAD-0006 Decision 4).
--
-- Pure-Lua emulation of KOReader's COMMON-CORE plugin API, mapped onto inkread's `inkread.*`
-- services. This re-implements the object/event scaffolding (EventListener/WidgetContainer OO,
-- UIManager, Menu registration, InfoMessage, Dispatcher, gettext, logger, LuaSettings) in Lua;
-- only leaf capabilities (show a message, read the doc/view) call into the Rust core.
--
-- A custom `require` resolves these modules. Default = STRICT: an unsupported module/symbol raises
-- a clear error (never a silent half-run). PROBE mode (dev) records the gap for the compatibility
-- matrix instead of failing. Mirrors KOReader's API as of the pinned version in the matrix doc.

local M = {} -- module registry: KOReader require-path -> module table

-- ===== EventListener / Widget OO (ui/widget/eventlistener.lua semantics) =====
local EventListener = {}
function EventListener:extend(subclass_prototype)
    local o = subclass_prototype or {}
    setmetatable(o, self)
    self.__index = self
    return o
end
function EventListener:new(o)
    o = o or {}
    setmetatable(o, self)
    self.__index = self
    if o.init then o:init() end
    return o
end
M["ui/widget/eventlistener"] = EventListener

local Widget = EventListener:extend{}
M["ui/widget/widget"] = Widget

-- WidgetContainer: plugins `:extend` this. (We omit child-layout; plugins only need the OO + events.)
local WidgetContainer = Widget:extend{}
M["ui/widget/container/widgetcontainer"] = WidgetContainer

-- ===== gettext: identity translator (no catalogs yet) =====
M["gettext"] = function(s) return s end

-- ===== logger -> inkread.log =====
local function join(...)
    local parts = {}
    for i = 1, select("#", ...) do parts[i] = tostring((select(i, ...))) end
    return table.concat(parts, " ")
end
M["logger"] = {
    info = function(...) inkread.log(join(...)) end,
    warn = function(...) inkread.log(join(...)) end,
    err = function(...) inkread.log(join(...)) end,
    dbg = function(...) end,
}

-- ===== InfoMessage / ConfirmBox: widgets carrying text; UIManager:show routes them out =====
local InfoMessage = Widget:extend{}
M["ui/widget/infomessage"] = InfoMessage
local ConfirmBox = Widget:extend{}
M["ui/widget/confirmbox"] = ConfirmBox

-- ===== UIManager: show/close/scheduleIn =====
local UIManager = {}
function UIManager:show(widget, _refresh)
    if widget and widget.text then inkread.ui.show_message(widget.text) end
    return widget
end
function UIManager:close(_widget) end
function UIManager:setDirty(_widget, _mode) end
function UIManager:scheduleIn(_sec, _fn) end -- no scheduler in the harness yet
function UIManager:nextTick(fn) if fn then fn() end end
M["ui/uimanager"] = UIManager

-- ===== Dispatcher: record registered gesture actions (gesture routing not emulated yet) =====
local Dispatcher = { _actions = {} }
function Dispatcher:registerAction(name, spec) self._actions[name] = spec end
M["dispatcher"] = Dispatcher

-- ===== custom require resolver (strict by default; probe records gaps) =====
__inkread_ko = { probe = false, probed = {} }

local function record(path)
    __inkread_ko.probed[path] = true
end

require = function(name) -- luacheck: ignore (intentional global override)
    local mod = M[name]
    if mod ~= nil then return mod end
    if __inkread_ko.probe then
        record(name)
        -- a proxy that records every accessed symbol so the matrix knows the exact surface needed
        return setmetatable({}, {
            __index = function(_, k)
                record(name .. "." .. tostring(k))
                return function() end
            end,
            __call = function() return nil end,
        })
    end
    error("inkread: KOReader API '" .. tostring(name) .. "' is not supported (RR14)")
end

-- ===== harness: load a plugin's main.lua, run lifecycle, expose its menu =====
function __inkread_ko.run(main_src, chunk_name)
    local chunk = assert(load(main_src, chunk_name or "koplugin/main.lua"))
    local PluginClass = chunk()
    -- The host injects ui/view/document into the plugin instance (as KOReader's ReaderUI does).
    local menu = { _plugins = {} }
    function menu:registerToMainMenu(p) table.insert(self._plugins, p) end
    local ui = { menu = menu }
    local instance = PluginClass:new{ ui = ui, view = {}, document = {} }
    local items = {}
    for _, p in ipairs(menu._plugins) do
        if p.addToMainMenu then p:addToMainMenu(items) end
    end
    __inkread_ko._last = { instance = instance, items = items }
    return true
end

-- Menu items as "key=text" lines (simple wire to Rust; avoids table marshaling).
function __inkread_ko.menu_keys()
    local out = {}
    if __inkread_ko._last then
        for k, v in pairs(__inkread_ko._last.items) do
            out[#out + 1] = k .. "=" .. tostring(v.text or "")
        end
    end
    return table.concat(out, "\n")
end

-- Fire a menu item's callback by key. Returns true if it ran.
function __inkread_ko.invoke(key)
    local it = __inkread_ko._last and __inkread_ko._last.items[key]
    if it and it.callback then it.callback(); return true end
    return false
end

-- Recorded probe gaps as newline-joined paths (for the compatibility matrix).
function __inkread_ko.probed_list()
    local out = {}
    for k in pairs(__inkread_ko.probed) do out[#out + 1] = k end
    table.sort(out)
    return table.concat(out, "\n")
end
