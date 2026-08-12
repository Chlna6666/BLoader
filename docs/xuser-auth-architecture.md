# XUser / Microsoft Runtime Auth Architecture

## 设计边界

BLoader 把“用户身份”和“Microsoft Runtime 认证能力”分成两个独立层：

1. **Virtual XUser Provider**：由 BLoader 拥有，向 Minecraft 暴露 BMCBL 选择的账号 B，包括 XUID、Gamertag、LocalId、生命周期与变更事件。
2. **Microsoft Auth Capability Adapter**：仅复用官方 Runtime 的 Device、Title、XSTS 与请求签名能力，不把 Windows native 用户 A 当成 B 的 backing identity。

## Token 语义

目标 pre-XSTS 聚合必须满足：

```text
DeviceToken = Microsoft Runtime official device credential
TitleToken  = Microsoft Runtime official Minecraft title credential
UserTokens  = BMCBL raw UToken(B)

XSTS = Microsoft XSTS authorize(DeviceToken, TitleToken, UserTokens(B))
```

因此不能在最终 XSTS 已经产生之后再替换 UToken，也不能直接复用不同账号 A 的最终 XSTS。

## 路由规则

### Windows native 用户与 B 相同

BLoader 可使用经过 XUID 二次验证的 native XUser 作为 **same-identity capability fast path**。Minecraft 仍看到 synthetic XUser，native handle 不对外暴露。

### Windows native 用户 A 与 B 不同

A 不能进入正常最终 Token 返回路径。A 只允许临时触发 pre-XSTS capability probe，以帮助定位 Microsoft Runtime 的内部聚合 ABI；其最终 Token 结果必须丢弃。

### Windows 没有系统 Xbox 用户

Virtual XUser(B) 仍然成立。认证层必须最终能够直接调用/驱动 pre-XSTS capability path，而不能依赖 interactive `XUserAddAsync` 建立 backing user。ABI 未解析前保持 fail-closed。

## 不变量

- Minecraft 的公开 XUser 身份始终是 BLoader Virtual XUser(B)。
- 不同 XUID 的 native handle 永远不能进入正常 `XUserGetTokenAndSignature*` 返回路径。
- native capability probe 永远不把最终 XSTS 返回给 Minecraft。
- BMCBL 只提供 B 的原始 UToken；refresh token 不进入 Minecraft 进程。
- Microsoft Runtime 继续负责官方 DeviceToken、TitleToken、XSTS 与 Signature。
- pre-XSTS 注入必须发生在 `xsts/authorize` 的 `UserTokens` 聚合之前。
