# Changelog

本项目遵循语义化版本。每次 BLoader 行为或 ABI 发生变化时必须更新小版本号和本文件。

## [0.2.4] - 2026-07-31

### Added

- 在 BLoader 内部实现按需 XUser 登录桥，不再依赖独立 `xgameruntime.dll` 代理。
- 仅在收到 BMCBL 为当前 Minecraft PID 创建的一次性认证管道后，Hook Microsoft 官方 `xgameruntime.dll!QueryApiImpl`。
- 内置 XUser 50 槽 ABI、Gamertag 接口、XUserAdd、用户句柄、XUID、年龄组和权限检查。
- 内置 XAsync provider，异步任务继续调用 Microsoft 官方 `IXThreadingImpl`。
- 内置 URL 到 Xbox Live、PlayFab、Multiplayer、Realms 和 Licensing relying-party 的 Token 路由。
- 为 ANSI 和 UTF-16 `XUserGetTokenAndSignatureAsync` 实现完整结果缓冲区。
- 使用 BMCBL 提供的 P-256 BCRYPT 私钥 blob，在进程内导入不可导出的 BCrypt 句柄。
- 按 Xbox Proof-of-Possession 规则生成 SHA-256 + ECDSA P-256 `Signature`，修复 Presence、好友状态和依赖签名的 Xbox Live 请求。
- 添加管道魔数、协议版本、目标 PID、父进程 PID、有效期、长度和 SHA-256 校验。
- 添加 Windows Release 构建、测试、导出检查、打包和标签发布工作流。

### Changed

- 第三方 native/preload Mod 不再在 `DllMain` loader lock 中加载。
- BLoader bootstrap 先处理一次性登录会话，再加载任何第三方 Mod。
- 除 `QueryApiImpl` 外，所有 XGameRuntime API 继续由系统官方 `xgameruntime.dll` 实现。
- 未检测到有效会话时，BLoader 不创建 MinHook、不修改官方函数，并保持原版 Xbox 登录行为。

### Security

- 登录 Token、设备私钥和启用开关不再通过 Minecraft 环境变量、命令行、注册表或临时 JSON 文件传递。
- 管道拒绝远程客户端，使用受限 DACL，并双向校验 BMCBL 父进程 PID 与 Minecraft 客户端 PID。
- 传输载荷、私钥 blob、请求正文、Authorization 和 Signature 临时缓冲区在使用后清零。
- 日志不输出 Gamertag、XUID、Token、私钥、管道载荷或完整请求正文。

## [0.2.3] - 2026-07-31

- 首个公开轻量化版本。
- 默认不编译 ArcUI 面板和 Minecraft 符号包子系统。
- 保留原生预加载、热加载、崩溃捕获、stdio 捕获和 Mod 诊断框架。
