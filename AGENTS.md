# Moodflow 设置端

这是独立的 GPL-3.0-or-later 程序，保留 LICENSE 和上游署名。不要把上游未声明许可的历史版本当作 MIT。当前不合并上游；修改必须对应相邻 Moodflow 主仓的配置与 RPC 契约。

不写正文日志，不在测试中注册输入法或改真实用户配置；临时数据限定到缓存目录。测试快照由当前 Moodflow 核心生成，较新可选契约的测试夹具在 compat_ui/，生产 mock 不使用它们。触发键拼写对齐 moodflow-keys KEY_TABLE。

运行 cargo fmt、cargo test --bin wind_setting 和 cargo clippy --all-targets。提交使用显式文件路径，不卷入其他会话的文件。
