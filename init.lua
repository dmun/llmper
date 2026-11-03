-- llemper config — edit this file to change anything.
-- Each global falls back to an env var, then to the value below.

llm_url = os.getenv("LLM_URL") or "https://api.deepseek.com/chat/completions"
llm_model = os.getenv("LLM_MODEL") or "deepseek-chat"
llm_api_key = os.getenv("LLM_API_KEY") or os.getenv("DEEPSEEK_API_KEY")

llm_system = os.getenv("LLM_SYSTEM") or "You are a concise assistant."

-- Other OpenAI-compatible endpoints:
--   llm_url = "https://api.openai.com/v1/chat/completions"
--   llm_model = "gpt-4o-mini"
--   llm_url = "http://localhost:11434/v1/chat/completions"
--   llm_model = "llama3.2"
