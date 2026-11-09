# Moodflow 设置程序

Moodflow Windows 输入法的独立原生设置程序，基于 windui 0.31.0，通过 Moodflow JSON-RPC 控制和事件管道通信。安装、便携、自定义数据目录和开发版使用与核心相同的目录与变体规则。

需要 Rust 2024 / Windows MSVC。将本仓放在 `Moodflow` 主仓旁，设置 `MOODFLOW_APP_VERSION` 后执行 `cargo build --release`。主仓 `scripts/dev.ps1` 自动注入产品版本并收集产物，保留 `wind_setting.exe` 文件名供既有核心调用。

设置清单与测试快照已对应 Moodflow 核心。原项目较新版本的窗口规则等可选能力仅在核心实际声明支持时显示，旧核心不写入它们。可通过 `MOODFLOW_RPC_MOCK=1` 离线查看合成界面。

验证：`cargo fmt --check`、`cargo test --bin wind_setting`。测试不需要 AI 密钥。

本程序基于 huanfeng/wind-setting 的 GPL-3.0-or-later 发布版本独立适配，继续遵循该许可。原始许可证见 LICENSE，历史说明见 UPSTREAM-README.md；对应源码与修改说明见 [MOODFLOW-SOURCE.md](MOODFLOW-SOURCE.md)。本 fork 不要求签署原项目 CLA。
