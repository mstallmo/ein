# Providers

Ein ships with four model client plugins. Set `model_client_name` to the plugin name to activate it.

## OpenRouter

[OpenRouter](https://openrouter.ai) provides access to hundreds of models from many providers through a single API key.

```json
{
  "model_client_name": "ein_openrouter",
  "plugin_configs": {
    "ein_openrouter": {
      "params": {
        "api_key": "sk-or-...",
        "base_url": "https://openrouter.ai/api/v1",
        "model": "anthropic/claude-sonnet-4-5"
      }
    }
  }
}
```

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `api_key` | yes | — | Your OpenRouter API key |
| `base_url` | no | `https://openrouter.ai/api/v1` | API base URL |
| `model` | no | provider default | Model identifier (e.g. `anthropic/claude-opus-4-7`) |

## Anthropic

Direct access to Claude models via the Anthropic API.

```json
{
  "model_client_name": "ein_anthropic",
  "plugin_configs": {
    "ein_anthropic": {
      "params": {
        "api_key": "sk-ant-...",
        "model": "claude-opus-4-7"
      }
    }
  }
}
```

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `api_key` | yes | — | Your Anthropic API key |
| `model` | no | provider default | Model identifier (e.g. `claude-sonnet-4-6`) |

There is no `base_url` parameter for the Anthropic plugin; it always uses the official Anthropic API endpoint.

## OpenAI

For OpenAI's API or any OpenAI-compatible endpoint (local models via llama.cpp, vLLM, LM Studio, etc.).

```json
{
  "model_client_name": "ein_openai",
  "plugin_configs": {
    "ein_openai": {
      "params": {
        "api_key": "sk-...",
        "base_url": "https://api.openai.com/v1",
        "model": "gpt-4o"
      }
    }
  }
}
```

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `api_key` | yes | — | Your OpenAI API key (or a dummy value for local servers) |
| `base_url` | no | OpenAI's endpoint | API base URL; change this to point at a local server |
| `model` | no | provider default | Model identifier |

## Ollama

Run models locally with [Ollama](https://ollama.com). No API key required.

```json
{
  "model_client_name": "ein_ollama",
  "plugin_configs": {
    "ein_ollama": {
      "params": {
        "base_url": "http://localhost:11434",
        "model": "llama3"
      }
    }
  }
}
```

| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `base_url` | no | `http://localhost:11434` | URL of the Ollama server |
| `model` | no | Ollama default | Model name as known to Ollama (e.g. `llama3`, `mistral`) |

Make sure Ollama is running and the model is pulled before starting Ein:

```bash
ollama pull llama3
ollama serve
```

## Switching providers mid-session

Edit `~/.ein/config.json` and change `model_client_name` and the relevant `plugin_configs` entry. Ein detects the file change and sends the new config to the server automatically. The active conversation history is preserved; the next prompt will use the new model.
