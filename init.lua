-- llemper config — edit this file to change anything.

llm_url = os.getenv("LLM_URL") or "https://api.deepseek.com/chat/completions"
llm_model = os.getenv("LLM_MODEL") or "deepseek-chat"
llm_api_key = os.getenv("LLM_API_KEY") or os.getenv("DEEPSEEK_API_KEY")
llm_system = os.getenv("LLM_SYSTEM") or "You are a concise assistant."

-- UI. Each slot takes a component:
--   render = "name"                 -> built-in renderer ("transcript", "textarea", "status")
--   render = function(state, w, h)  -> returns a string to draw (nil draws nothing)
--   key    = function(key)          -> return true to swallow a key, false to fall through
--                                      to the built-in handler (only meaningful for "textarea")
ui = {
  main = { render = "transcript" },
  input = { render = "textarea" },
  status = { render = "status" },
}

-- Examples:
--
-- Rebind the input: intercept ctrl-n, let everything else behave normally.
-- ui = {
--   input = {
--     render = "textarea",
--     key = function(k)
--       if k == "ctrl-n" then return true end
--       return false
--     end,
--   },
-- }
--
-- Custom status bar in Lua.
-- ui = {
--   status = {
--     render = function(state)
--       return state.model .. " | " .. state.messages .. " msgs"
--     end,
--   },
-- }
