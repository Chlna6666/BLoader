# Motion Blur

`examples/bl_motion_blur` 是一个真正独立的 `D3D12` 动态模糊 mod。

它现在会把控制项直接注册到 BLoader 宿主 UI 面板中，并通过 `.lang` 文件向宿主注册本地化文案。

## 实现方式

- mod 在 `register_d3d12_render_callback` 中拿到真实 `device / command_list / back buffer`。
- 每帧把当前 back buffer 复制进历史纹理队列。
- 再按“越旧越透明”的曲线把历史纹理直接叠回当前 render target。
- resize 时会清历史并短暂跳帧，避免 swapchain 重建阶段崩溃。

透明度曲线使用类似参考实现的衰减方式：

```text
opacity = max_opacity * pow(age_factor, 1.35)
```

## 宿主 UI 面板

加载器面板默认快捷键是 `Alt + M`。

打开宿主面板后，可以看到该 mod 注册的 `Motion Blur` 窗口，提供：

- 启用/禁用
- 历史帧数量
- 强度
- 清空历史帧缓存

另外，该 mod 仍然保留了 `动态模糊` 的功能开关注册，宿主可以继续直接回调启停状态。

## 本地化

示例在 `lang/en_US.lang` 和 `lang/zh_CN.lang` 中提供文案。

`on_load` 时会调用：

- `bl_sdk::i18n::register_lang("zh_CN", include_str!("../lang/zh_CN.lang"))`
- `bl_sdk::i18n::register_lang("en_US", include_str!("../lang/en_US.lang"))`

之后在 `ui_panel` 回调里直接使用 `bl_sdk::i18n::tr("motion_blur.ui.title")` 取当前宿主语言下的文本。

## 说明

- `H`: 开关模糊
- `J`: 减少历史帧数
- `K`: 增加历史帧数
- `L`: 循环切换强度

## 依赖

- `bl-sdk`
- `windows`

## 备注

加载器只提供通用 DX12 渲染回调和宿主 UI 注册入口。
motion blur 的历史帧管理、资源重建、shader 和叠加逻辑都在这个 mod 自己内部。
