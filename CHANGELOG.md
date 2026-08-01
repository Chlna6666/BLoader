# Changelog

本项目遵循语义化版本。每次 BLoader 行为、ABI、安全协议或发布格式发生变化时，必须更新版本号或本文件。

## [0.2.9] - 2026-08-01

### Added

- 为 ANSI 与 UTF-16 `XUserGetTokenAndSignatureAsync` 增加完整请求头复制，不再只检查调用方指针后丢弃 Header 内容。
- 增加签名策略模型，按策略规定的 Header 名称顺序、忽略大小写匹配 Header 值；策略要求但请求未携带的 Header 使用空值占位，保持 Proof-of-Possession 输入布局稳定。
- 内置 Xbox 默认 NSAL 策略：普通 `*.xboxlive.com` 服务使用签名版本 1、`MaxBodyBytes=8192`、无 `ExtraHeaders`；`device.mgt.xboxlive.com` 与 `data-vef.xboxlive.com` 使用完整请求正文。
- 增加请求头顺序、大小写、缺失占位、未知服务兼容回退和 CR/LF Header 注入拒绝测试。

### Changed

- 签名正文现在按策略的 `MaxBodyBytes` 截断，不再无条件签入完整请求正文。
- 对尚未提供 Partner Center Title NSAL 元数据的非 Xbox 自定义服务，暂按调用方 Header 顺序兼容回退，并排除 `Authorization`、`Signature`、`Host` 与 `Content-Length` 等派生 Header。
- BLoader 版本更新为 `0.2.9`。

### Fixed

- 修复 Token Provider 接收 `header_count` 与 `headers` 后只验证、不保存，导致非空 `ExtraHeaders` 签名策略无法工作的实现缺口。
- 修复签名 Header 名称大小写不同或策略 Header 缺失时，无法按照 Xbox Signing Policy 生成确定性字段序列的问题。
- 明确 Presence 默认 Xbox 签名策略的 `ExtraHeaders` 为空：`x-xbl-contract-version`、`Content-Type` 和 `Accept-Language` 等 Header 仍随 HTTP 请求发送，但不会被错误地强行加入默认 PoP Signature。

### Security

- Header 名称和值均有长度上限；Header 名称必须符合 HTTP token 字符集，值拒绝 CR、LF 与 NUL，防止 Header 注入。
- 复制到异步上下文中的策略 Header 值和请求正文会在完成或释放时清零；日志仍不记录 Header 值、Token、Authorization、Signature 或请求正文。

## [0.2.8] - 2026-08-01

### Added

- 新增进程内 Xbox Rich Presence Bridge，在自定义 XUser 会话启用时动态扫描并 Hook 已加载 XSAPI 模块导出的 `XblPresenceSetPresenceAsync`。
- 直接复用 Minecraft 传入的 SCID、Presence ID、Presence Token IDs 和 active/inactive 状态，不依赖游戏内部类、偏移或版本特定地图状态 Hook。
- 使用 BMCBL 注入账户的 Xbox Live XSTS、UHS 和 P-256 Proof-of-Possession 私钥，通过 WinHTTP 向 `userpresence.xboxlive.com` 独立提交原始 Rich Presence。
- Presence Bridge 使用 Microsoft 官方 `IXThreadingImpl` 驱动调用方的 `XAsyncBlock`，避免官方 XSAPI 因自定义 XUser 上下文失败而阻断游戏状态更新。
- 增加 XSAPI 导出发现、Hook 地址、HTTP 状态码、Rich Presence 参数数量和失败原因诊断；日志不包含 Token、Authorization、Signature 或请求正文。
- 增加 Presence JSON 结构单元测试，验证输出与 XSAPI `TitleRequest` 的 `state/activity/richPresence/id/scid/params` 格式一致。

### Changed

- 自定义 XUser 首次被查询时启动 Presence Bridge 后台 Hook 与发送线程；无 BMCBL 安全会话时不会启动或修改 XSAPI。
- Presence 更新采用 latest-wins 队列，避免游戏短时间连续切换界面或世界时堆积过时请求。
- BLoader 版本更新为 `0.2.8`。

### Fixed

- 修复使用 `QueryApiImpl` 替换 XUser 后，好友状态只能显示“正在玩 Minecraft”，无法显示主菜单、世界或地图等 Rich Presence 详细状态的问题。
- 修复官方 XSAPI Presence 上下文与 BMCBL 自定义 XUser 不完全兼容时，`XblPresenceSetPresenceAsync` 异步失败并停止详细状态上报的问题。

### Security

- Presence Bridge 只在已验证的一次性 BMCBL XUser 会话存在时启用。
- Xbox Token、UHS、Authorization 和 Signature 只保存在进程内临时缓冲区，并使用 `Zeroizing` 在释放时清理。
- Bridge 仅记录公开模块名、函数地址、HTTP 状态和参数计数，不记录 XUID、Token、私钥、Authorization、Signature 或 Presence 请求正文。

## [0.2.7] - 2026-07-31

### Added

- 将 `DllMain` 阶段产生的 XUser Bridge 入口、管道验证、系统 Runtime 和 Hook 安装状态缓存为安全诊断记录。
- 在正常日志系统和调试控制台就绪后回放早期诊断，使 `latest.log` 和控制台都能直接看到桥接结果。
- 最早构建标记新增 `xuser_bridge=embedded protocol=1`，便于识别实际加载的 DLL 是否包含内置 XUser Bridge。

### Fixed

- 修复桥接入口已经执行但日志只存在于 `bootstrap.marker.log`，导致主日志中看不到 Xbox Gamertag、系统 `xgameruntime.dll` 路径和 `QueryApiImpl` Hook 状态的问题。
- 修复用户仅根据常规控制台日志无法区分“未执行桥接”“桥接失败”和“Hook 尚未首次命中”的诊断歧义。

### Security

- 回放队列只保存经过清理的 Gamertag、系统 DLL 路径、函数地址和状态文本；仍不保存或输出 Token、XUID、私钥、Authorization、Signature、请求正文或原始 IPC 载荷。

## [0.2.6] - 2026-07-31

### Added

- XUser Bridge 在 `DllMain` 最早阶段输出明确入口标记，包括协议版本、管道门控模式和仅接管 `QueryApiImpl` 的范围。
- 有效 BMCBL 会话存在时，记录系统原生 `xgameruntime.dll` 的实际来源、完整路径、`QueryApiImpl` 地址和 MinHook trampoline 地址。
- 首次命中 `QueryApiImpl` Hook 和首次请求 `CLSID_XUserImpl` 时输出一次性诊断日志，便于区分“Hook 已安装”和“游戏实际调用了 Hook”。
- 无安全管道时明确记录系统原生 `xgameruntime.dll` 当前是否已由宿主映射，以及游戏将继续使用微软官方 XUser 登录。

### Changed

- 仅在 BMCBL 安全会话已通过 PID、时效和摘要验证后，若官方 Runtime 尚未映射，才从 `System32` 同步加载微软原生 `xgameruntime.dll` 并安装 Hook。
- 无会话启动仍保持零主动 Runtime 加载、零 MinHook 和零自定义 XUser 状态。
- 拒绝对非 `System32` 路径的 `xgameruntime.dll` 安装 Hook，防止本地同名 DLL 劫持。
- BLoader 版本更新为 `0.2.6`。

### Fixed

- 修复只有“安全管道传输完成”但缺少 BLoader 端 Runtime/Hook 状态，导致无法判断桥接入口、原生 Runtime 和 Hook 是否实际工作的诊断缺口。

### Security

- 新增日志只输出公开 Gamertag、系统 DLL 路径和函数地址，不输出 XUID、Token、私钥、Authorization、Signature、请求正文或管道原始载荷。

## [0.2.5] - 2026-07-31

### Changed

- 未检测到当前 Minecraft PID 对应的 BMCBL 安全一次性管道时，明确记录“不安装 `QueryApiImpl` Hook，继续使用微软官方 XUser 登录”。
- BMCBL 安全会话验证失败或 Hook 安装失败时，明确记录官方登录回退状态，避免用户误判为账号注入失败后仍在接管官方 Runtime。
- 安全会话接收并成功安装 Hook 后，仅记录经过控制字符过滤和 64 字符长度限制的公开 Xbox Gamertag。
- BLoader 版本更新为 `0.2.5`。

### Security

- 无有效 BMCBL 会话时仍保持零 Hook、零自定义 XUser 状态，不修改微软官方 `QueryApiImpl`。
- 日志继续禁止输出 XUID、Token、私钥、Authorization、Signature、完整请求正文和原始管道载荷。

## [0.2.4] - 2026-07-31

### Added

- 在 BLoader 内部实现按需 XUser 登录桥，不再依赖独立 `xgameruntime.dll` 代理。
- 仅在收到 BMCBL 为当前 Minecraft PID 创建的一次性认证管道后，Hook Microsoft 官方 `xgameruntime.dll!QueryApiImpl`。
- 内置 XUser 50 槽 ABI、Gamertag 接口、XUserAdd、用户句柄、XUID、年龄组和权限检查。
- 内置 XAsync Provider，异步任务继续调用 Microsoft 官方 `IXThreadingImpl`。
- 内置 URL 到 Xbox Live、PlayFab、Multiplayer、Realms 和 Licensing relying-party 的 Token 路由。
- 为 ANSI 和 UTF-16 `XUserGetTokenAndSignatureAsync` 实现完整结果缓冲区。
- 使用 BMCBL 提供的 P-256 BCRYPT 私钥 Blob，在进程内导入不可导出的 BCrypt 句柄。
- 按 Xbox Proof-of-Possession 规则生成 SHA-256 + ECDSA P-256 `Signature`，修复 Presence、好友状态和依赖签名的 Xbox Live 请求。
- 添加管道魔数、协议版本、目标 PID、父进程 PID、有效期、长度和 SHA-256 校验。
- 添加 Windows Release 构建、测试、导出检查、打包和标签发布工作流。
- 发布 ZIP 新增 `manifest.json`、`BLoader.dll.sha256`，并在 ZIP 外生成独立 `.zip.sha256` 文件。
- README 新增完整架构、安全边界、GDK 支持范围、Signature、CI 和发布产物说明。
- XUser Bridge 成功日志记录经过控制字符过滤和长度限制的公开 Gamertag，便于确认启动账号。

### Changed

- 第三方 native/preload Mod 不再在 `DllMain` loader lock 中加载。
- BLoader bootstrap 先处理一次性登录会话，再加载任何第三方 Mod。
- 除 `QueryApiImpl` 外，所有 XGameRuntime API 继续由系统官方 `xgameruntime.dll` 实现。
- 未检测到有效会话时，BLoader 不创建 MinHook、不修改官方函数，并保持原版 Xbox 登录行为。
- Windows CI 使用 `Swatinem/rust-cache` 缓存 Cargo registry、Git 依赖和可复用 `target` 缓存；缓存键包含 Rust 环境与 Cargo 清单变化。
- `dumpbin.exe` 不再硬编码 Visual Studio 2022 路径，优先使用 PATH，再通过 `vswhere.exe` 定位当前 Runner 的 MSVC 工具链。
- GitHub 标签发布同时上传 ZIP 与 SHA-256 文件，CHANGELOG 不再作为二进制附件上传。

### Fixed

- 修复 `windows-2025` Runner 使用新版 Visual Studio 时找不到 `dumpbin.exe` 导致的发布验证失败。
- 修复 PowerShell 兜底搜索 `dumpbin.exe` 时的空管道语法错误。
- 修复发布阶段只有 Rust 编译成功、但导出验证和 Artifact 打包失败的问题。
- 修复 PR Artifact 清单记录临时 merge commit 而非真实源码 head SHA 的问题。

### Security

- 登录 Token、设备私钥和启用开关不再通过 Minecraft 环境变量、命令行、注册表或临时 JSON 文件传递。
- 管道拒绝远程客户端，使用受限 DACL，并双向校验 BMCBL 父进程 PID 与 Minecraft 客户端 PID。
- 传输载荷、私钥 Blob、请求正文、Authorization 和 Signature 临时缓冲区在使用后清零。
- 日志只允许输出经过清理的公开 Gamertag；不记录 XUID、Token、私钥、Authorization、Signature、完整请求正文或原始管道载荷。
- 发布清单记录 DLL 大小、SHA-256、源码提交、工作流提交和 XUser Bridge 安全能力，便于 BMCBL 部署前校验。

## [0.2.3] - 2026-07-31

- 首个公开轻量化版本。
- 默认不编译 ArcUI 面板和 Minecraft 符号包子系统。
- 保留原生预加载、热加载、崩溃捕获、stdio 捕获和 Mod 诊断框架。