-- KOReader's bundled example plugin (faithful to plugins/hello.koplugin/main.lua), vendored
-- verbatim as a compatibility conformance fixture (RR14). If this runs unmodified on inkread's
-- KOReader shim, the common-core API surface is covered.
local Dispatcher = require("dispatcher") -- luacheck:ignore
local InfoMessage = require("ui/widget/infomessage")
local UIManager = require("ui/uimanager")
local WidgetContainer = require("ui/widget/container/widgetcontainer")
local _ = require("gettext")

local Hello = WidgetContainer:extend{
    name = "hello",
    is_doc_only = false,
}

function Hello:onDispatcherRegisterActions()
    Dispatcher:registerAction("helloworld_action", {category="none", event="HelloWorld", title=_("Hello World"), general=true,})
end

function Hello:init()
    self:onDispatcherRegisterActions()
    self.ui.menu:registerToMainMenu(self)
end

function Hello:addToMainMenu(menu_items)
    menu_items.hello_world = {
        text = _("Hello World"),
        -- in reader.koplugin, a menu_item with no sorting_hint goes to the "more_tools" menu
        sorting_hint = "more_tools",
        callback = function()
            UIManager:show(InfoMessage:new{
                text = _("Hello, plugin world"),
            })
        end,
    }
end

return Hello
