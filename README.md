# ⚡ LLM 并发测试工具 (llmperf)

一款基于 [Tauri 2](https://tauri.app/) 开发的跨平台桌面应用，用于对 LLM API（兼容 OpenAI 接口）进行**并发流式请求测试**，直观对比多路请求的表现。

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Tauri](https://img.shields.io/badge/Tauri-2.x-3c873a?logo=tauri)
![React](https://img.shields.io/badge/React-19-61dafb?logo=react)
![Rust](https://img.shields.io/badge/Rust-1.80+-orange?logo=rust)

## ✨ 功能特性

- **并发流式请求**：同时向 LLM API 发起 N 路流式请求，实时对比输出效果
- **实时流式展示**：通过 `stream_chunk` 事件实时推送 Token，前端即时渲染流式输出
- **灵活并发配置**：支持 1~50 路并发，窗口数量自动跟随并发数动态增减
- **多窗口对比**：以网格布局展示所有并发窗口的响应结果，方便横向对比
- **请求耗时统计**：每个窗口独立记录从发送到完成的总耗时（毫秒）
- **错误捕获与展示**：流式请求过程中的错误会实时推送到对应窗口并展示
- **跨平台支持**：基于 Tauri，同时支持 Linux、macOS 和 Windows
- **暗色主题**：精心设计的暗色 UI，视觉舒适

## 🖼 界面预览

应用主界面分为两部分：

- **顶部配置栏**：配置 Base URL、API Key、Model、并发数，以及输入测试消息
- **底部聊天窗口区**：以网格形式展示每个并发窗口的实时流式响应

支持 Enter 键快速发送消息，一键清空所有窗口。

## 🛠 技术栈

| 层级 | 技术 |
|------|------|
| 前端框架 | [React 19](https://react.dev/) + [TypeScript](https://www.typescriptlang.org/) |
| 构建工具 | [Vite 7](https://vitejs.dev/) |
| 桌面框架 | [Tauri 2](https://tauri.app/) (Rust) |
| HTTP 客户端 | [async-openai](https://crates.io/crates/async-openai) (OpenAI 兼容接口) |
| 异步运行时 | [tokio](https://tokio.rs/) (Tauri 内置) |
| 流式处理 | [futures](https://crates.io/crates/futures) |

## 📦 安装

### 前置依赖

- [Rust](https://rustup.rs/)（推荐通过 rustup 安装）
- [Node.js](https://nodejs.org/) ≥ 18
- npm ≥ 9

**Linux 额外依赖**（Ubuntu/Debian）：

```bash
sudo apt install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

## 🚀 快速开始

### 开发模式

```bash
# 安装依赖
npm install

# 启动开发服务器 + Tauri 窗口
npm run tauri dev
```

### 构建发布版本

```bash
# 构建前端 + Rust 后端
npm run build

# 构建linux版本
npm run tauri build -- --target x86_64-unknown-linux-gnu
```

构建产物位于 `src-tauri/target/release/bundle/` 目录下，具体取决于目标平台：

| 平台 | 格式 | 路径 |
|------|------|------|
| Linux | `.deb` / `.rpm` | `src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/` 或 `bundle/rpm/` |
| macOS | `.dmg` / `.app` | `src-tauri/target/release/bundle/dmg/` 或 `bundle/app/` |
| Windows | `.msi` / `.exe` | `src-tauri/target/release/bundle/msi/` 或 `bundle/nsis/` |

### 其他脚本

| 命令 | 说明 |
|------|------|
| `npm run dev` | 仅启动 Vite 前端开发服务器（无 Tauri 窗口） |
| `npm run build` | TypeScript 类型检查 + Vite 构建前端 |
| `npm run preview` | 预览构建后的前端产物 |
| `npm run tauri dev` | 启动 Tauri 开发模式（前端 + 桌面窗口） |
| `npm run tauri build` | 构建发布版桌面应用 |

## 📖 使用说明

1. **配置 API 端点**：填入 LLM API 的 Base URL（如 `http://127.0.0.1:16777/v1`）
2. **填入 API Key**：输入你的 API 密钥
3. **选择模型**：指定要测试的模型名称（如 `gpt-4o`、`qwen2.5-72b` 等）
4. **设置并发数**：1~50 之间的数字，表示同时发起的请求数
5. **输入消息**：在消息输入框中输入要测试的内容
6. **点击发送**：点击 🚀 发送按钮，或按 Enter 键

发送后，每个窗口会独立接收流式响应，完成后显示总耗时。

## 🏗 项目结构

```
llmperf/
├── src/                      # React 前端
│   ├── main.tsx              # 应用入口
│   ├── App.tsx               # 主组件（并发流式请求逻辑）
│   ├── App.css               # 暗色主题样式
│   └── assets/               # 静态资源
├── src-tauri/                # Tauri 后端 (Rust)
│   ├── src/
│   │   ├── main.rs           # Rust 入口
│   │   └── lib.rs            # Tauri 命令 & 并发请求逻辑
│   ├── Cargo.toml            # Rust 依赖
│   ├── tauri.conf.json       # Tauri 配置
│   └── build.rs              # 构建脚本
├── index.html                # HTML 入口
├── vite.config.ts            # Vite 配置
├── tsconfig.json             # TypeScript 配置
└── package.json              # Node.js 依赖
```

## 🔧 架构说明

```
┌─────────────── 前端 (React + Vite) ───────────────┐
│                                                     │
│  ┌─────────────┐  invoke  ────────────────────────▶ │  ┌───────────────────┐  │
│  │  配置 & UI   │                                   │  │  send_concurrent  │  │
│  │  消息输入    │  stream_chunk 事件 ◀───────────── │  │  _request (Rust)  │  │
│  │  窗口网格    │                                   │  │  (async-openai)   │  │
│  └─────────────┘                                   │  └───────────────────┘  │
│                                                    │          │               │
│                                                    │          ▼               │
│                                                    │  ┌───────────────────┐  │
│                                                    │  │  N 路并发请求      │  │
│                                                    │  │  (tokio spawn)     │  │
│                                                    │  └─────────┬─────────┘  │
│                                                    │            │             │
└────────────────────────────────────────────────────┘            ▼
                                                     ┌──────────────────┐
                                                     │  LLM API         │
                                                     │  (OpenAI 兼容)    │
                                                     └──────────────────┘
```

## 🤖 开发说明

本项目使用 **Vibe Coding** 方式开发完成，全程由 AI 辅助编码。

## 📄 许可证

[Apache License 2.0](LICENSE)
