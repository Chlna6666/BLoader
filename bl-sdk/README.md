# bl-sdk

`bl-sdk` 是编写 `MOD_TYPE = "BL"` 模组时使用的 Rust 辅助库。

## 提供的能力

- ABI 结构体与常量
- `Host` 包装器
- `project`/`tooling` 模块，用于从 mod `Cargo.toml` 生成清单与资源清单
- 注册辅助：
  - 事件
  - UI 面板
  - ArcUI 功能开关
  - 资源提供者
- Bedrock 原生 screen 注册与请求
- 路径与文本文件辅助函数
- `bl_export_mod!` 宏，用于导出 `bl_mod_main_v1`

## 典型使用流程

1. 实现 `on_load(host: &Host) -> i32`
2. 在 `on_load` 里注册事件、ArcUI 功能项、UI、资源回调；不要自行猜测或 hook 游戏内部地址
3. 用 `bl_export_mod!` 导出模组入口

## 最小示例

```rust
use bl_sdk::{bl_export_mod, Host};

fn on_load(host: &Host) -> i32 {
    host.info("loaded");
    0
}

fn on_unload() {}

bl_export_mod!(
    mod_id: "demo.min",
    mod_name: "Demo Min",
    on_load: on_load,
    on_unload: on_unload
);
```

## 建议

- 游戏运行时事件只有在当前版本的 `.blsym` 条目及 ABI 校验通过后才会由加载器提供；mod 应处理事件暂不可用的情况
- 把长期状态放在原子类型、`Mutex` 或 `OnceLock` 里
- UI 回调和事件回调里不要做长时间阻塞
- 需要写文件时优先使用宿主提供的 `BL_PATH_CACHE_DIR`
- 推荐在 `[package.metadata.bl]` 中维护 `mod_id`、`mod_name`、`resource_dirs`
- 需要游戏内部符号时，在 `[package.metadata.bl]` 设置 `requires_symbol_pack = true`，并用 `required_symbols = ["namespace.symbol"]` 声明依赖；加载器会在加载 DLL 前检查匹配的 `.blsym` 包。

## 公共符号接口

符号包由 BLoader 在启动期读取并验证，MOD 不需要读取文件或处理版本匹配：

```rust
let ready = bl_sdk::mapping::ready();
let pack_id = bl_sdk::mapping::pack_id();
let public_symbols = bl_sdk::mapping::public_symbols();
let module_base = bl_sdk::mapping::resolve("runtime.game_module_base");
```

`resolve` 仅返回符号包中 `expose_to_mods = true` 的已验证地址；未匹配、未公开或未解析的条目返回 `0`。

## ClientInstance 与 LocalPlayer

MOD 通过只读快照查询当前客户端状态，而不应保存跨世界生命周期的裸指针：

```rust
let client = bl_sdk::client::snapshot();
if let Some(player) = client.local_player {
    // 只在当前回调内使用；切换世界后重新读取。
}
```

`bl_sdk::client::ready()` 只表示已捕获 `ClientInstance`，`local_player_ready()` 才表示已捕获 `LocalPlayer`。在没有匹配 `.blsym` 包或尚未建立世界时，快照为空且 `status()` 返回 `unavailable`。
- 如果 mod 只需要一个总开关，优先注册 ArcUI 功能项，而不是自己创建宿主 UI 面板
