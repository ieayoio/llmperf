# ⚡ LLM Concurrency Testing Tool (llmperf)

> **Switch Language:** **中文** | [English](README.en.md)

A cross-platform desktop application based on [Tauri 2](https://tauri.app/) for **concurrent streaming request testing** of LLM APIs (compatible with OpenAI interface), allowing intuitive comparison of multiple request performances.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Tauri](https://img.shields.io/badge/Tauri-2.x-3c873a?logo=tauri)
![React](https://img.shields.io/badge/React-19-61dafb?logo=react)
![Rust](https://img.shields.io/badge/Rust-1.80+-orange?logo=rust)

## ✨ Features

- **Concurrent Streaming Requests**: Send N streaming requests to LLM API simultaneously, comparing output effects in real-time
- **Real-time Streaming Display**: Receive Token pushes via `stream_chunk` events, rendering streaming output instantly on the frontend
- **Flexible Concurrency Configuration**: Support 1~50 concurrent requests, with window count dynamically adjusting to match
- **Multi-window Comparison**: Display all concurrent window responses in a grid layout for easy horizontal comparison
- **Request Timing Statistics**: Each window independently records total elapsed time (milliseconds) from send to completion
- **Error Capture & Display**: Errors during streaming requests are pushed to corresponding windows in real-time
- **Internationalization Support**: Built-in Chinese and English (zh/en) internationalization with language switching
- **Cross-platform Support**: Based on Tauri, supporting Linux, macOS, and Windows
- **Dark Theme**: Carefully designed dark UI for comfortable viewing

## 🖼 Interface Preview

The main application interface is divided into two parts:

- **Top Configuration Bar**: Configure Base URL, API Key, Model, concurrency count, and input test messages
- **Bottom Chat Window Area**: Display real-time streaming responses from each concurrent window in grid format

Supports Enter key for quick message sending, and one-click clear all windows.

## 🛠 Tech Stack

| Layer | Technology |
|-------|------------|
| Frontend Framework | [React 19](https://react.dev/) + [TypeScript](https://www.typescriptlang.org/) |
| Build Tool | [Vite 7](https://vitejs.dev/) |
| Desktop Framework | [Tauri 2](https://tauri.app/) (Rust) |
| HTTP Client | [async-openai](https://crates.io/crates/async-openai) (OpenAI compatible interface) |
| Async Runtime | [tokio](https://tokio.rs/) (built into Tauri) |
| Streaming Processing | [futures](https://crates.io/crates/futures) |

## 📦 Installation

### Prerequisites

- [Rust](https://rustup.rs/) (recommended via rustup)
- [Node.js](https://nodejs.org/) ≥ 18
- npm ≥ 9

**Linux Additional Dependencies** (Ubuntu/Debian):

```bash
sudo apt install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

## 🚀 Quick Start

### Development Mode

```bash
# Install dependencies
npm install

# Start development server + Tauri window
npm run tauri dev
```

### Build Release Version

```bash
# Build frontend + Rust backend
npm run build

# Build Linux version
npm run tauri build -- --target x86_64-unknown-linux-gnu
```

Build artifacts are located in `src-tauri/target/release/bundle/` directory, depending on the target platform:

| Platform | Format | Path |
|----------|--------|------|
| Linux | `.deb` / `.rpm` | `src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/` or `bundle/rpm/` |
| macOS | `.dmg` / `.app` | `src-tauri/target/release/bundle/dmg/` or `bundle/app/` |
| Windows | `.msi` / `.exe` | `src-tauri/target/release/bundle/msi/` or `bundle/nsis/` |

### Other Scripts

| Command | Description |
|---------|-------------|
| `npm run dev` | Start Vite frontend dev server only (no Tauri window) |
| `npm run build` | TypeScript type check + Vite frontend build |
| `npm run preview` | Preview built frontend artifacts |
| `npm run tauri dev` | Start Tauri development mode (frontend + desktop window) |
| `npm run tauri build` | Build release desktop application |

## 📖 Usage Guide

1. **Configure API Endpoint**: Enter the LLM API Base URL (e.g., `http://127.0.0.1:16777/v1`)
2. **Enter API Key**: Input your API key
3. **Select Model**: Specify the model name to test (e.g., `gpt-4o`, `qwen2.5-72b`, etc.)
4. **Set Concurrency**: Enter a number between 1~50, representing the number of simultaneous requests
5. **Input Message**: Type the test content in the message input box
6. **Click Send**: Click the 🚀 send button, or press Enter key

After sending, each window will independently receive streaming responses and display total elapsed time upon completion.

## 🏗 Project Structure

```
llmperf/
├── src/                      # React Frontend
│   ├── main.tsx              # Application entry
│   ├── App.tsx               # Main component (concurrent streaming request logic)
│   ├── App.css               # Dark theme styles
│   ├── i18n/                 # Internationalization support
│   │   ├── index.ts          # i18n configuration
│   │   └── locales/          # Locale files
│   │       ├── zh.json       # Chinese
│   │       └── en.json       # English
│   └── assets/               # Static assets
├── src-tauri/                # Tauri Backend (Rust)
│   ├── src/
│   │   ├── main.rs           # Rust entry
│   │   └── lib.rs            # Tauri commands & concurrent request logic
│   ├── Cargo.toml            # Rust dependencies
│   ├── tauri.conf.json       # Tauri configuration
│   └── build.rs              # Build script
├── index.html                # HTML entry
├── vite.config.ts            # Vite configuration
├── tsconfig.json             # TypeScript configuration
├── README.md                 # Chinese documentation
├── README.en.md              # English documentation
└── package.json              # Node.js dependencies
```

## 🔧 Architecture Overview

```
┌─────────────── Frontend (React + Vite) ───────────────┐
│                                                        │
│  ┌─────────────┐  invoke  ──────────────────────────▶ │  ┌───────────────────┐  │
│  │  Config & UI  │                                     │  │  send_concurrent  │  │
│  │  Message Input│  stream_chunk event ◀────────────── │  │  _request (Rust)  │  │
│  │  Window Grid  │                                     │  │  (async-openai)   │  │
│  └─────────────┘                                     │  └───────────────────┘  │
│                                                      │          │               │
│                                                      │          ▼               │
│                                                      │  ┌───────────────────┐  │
│                                                      │  │  N Concurrent     │  │
│                                                      │  │  Requests         │  │
│                                                      │  │  (tokio spawn)    │  │
│                                                      │  └─────────┬─────────┘  │
│                                                      │            │             │
└──────────────────────────────────────────────────────┘            ▼
                                                       ┌──────────────────┐
                                                       │  LLM API         │
                                                       │  (OpenAI Compat)  │
                                                       └──────────────────┘
```

## 🤖 Development Notes

This project was developed using **Vibe Coding** methodology, with full AI-assisted coding throughout.

## 📄 License

[Apache License 2.0](LICENSE)
