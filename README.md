# Ein

## Getting Started

- [Signup for OpenRouter](https://openrouter.ai/)
- [Create OpenRouter API Key](https://openrouter.ai/settings/keys)

**Install Rust**
```bash
$ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Install WASM compile target**
```bash
$ rustup target add wasm32-wasip2
```

**Build Plugins**
```bash
$ ./scripts/build_install_plugins.sh
```

**Run Ein**
```bash
$ OPENROUTER_API_KEY=<your-openrouter-api-key> cargo run --release -- --prompt "Create a python file that prints hello world. The file should be placed in the app directory and called main.py"
```
