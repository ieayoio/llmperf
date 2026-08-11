# 项目概述
LLM 并发测试工具

# 项目介绍
- 一款基于 [Tauri 2](https://tauri.app/) 开发的跨平台桌面应用，用于对 LLM API（兼容 OpenAI 接口）进行行鞥测试
- 本项目前端代码在`src`中，后端代码在`src-tauri`中

# 代码规则
- 代码需要添加必要的中文注释，方便理解和阅读
- 避免代码产生冗余，添加必要的函数、方法、类使项目更具有结构化
- 如果我没有明确让你git提交或者推送，你不要主动去提交或者推送代码

# 测试api
为了方便你做测试下面提供一个大模型api给你做测试，你可以实际调用来验证
baseurl: http://127.0.0.1:16777/v1
apikey: sk-bL64JzHBLHJoiahD619Fx9D4KrM9cTq2rY3B0puH60VTbbgx
模型：testllm、testllm-nk
其中testllm-nk表示无思考模型，testllm可能会有思考