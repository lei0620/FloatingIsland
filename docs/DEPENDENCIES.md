# 依赖与第三方许可

岛仔原创代码采用 MIT。第三方依赖、字体、图标及品牌标识遵循各自许可；MIT 不授予第三方商标权。完整归属及许可文本见 [THIRD_PARTY_NOTICES.txt](../THIRD_PARTY_NOTICES.txt)。

## 本地补丁

- `vendor/eframe`：基于 0.31.1，处理 Windows WGL 上下文复用、岛体缩放、窗口焦点和透明窗口，并保留 RGB 字体覆盖补丁。
- `vendor/egui`：基于 0.31.1，为不同缩放倍率维护独立的字体纹理。
- `vendor/epaint`：基于 0.31.1，在 Windows 用 DirectWrite 保留真实字体的灰度及 RGB 覆盖，维持原有排版、绘制边界和字体图集；不可用时回退原栅格器。范围及验证入口见 [补丁说明](../vendor/epaint/DAOZAI_PATCH.md)。
- `vendor/egui_glow`：基于 0.31.1，保留 Windows RGB 字体覆盖并通过双源混合绘制不透明窗口；透明窗口及不支持的驱动回退灰度。字体按前景色校正，图像纹理维持原路径。见 [补丁说明](../vendor/egui_glow/DAOZAI_PATCH.md)。
- `vendor/imap`：基于 2.4.1，关闭 `nom` 默认功能并保留 `std`，移除邮件协议解析不需要的 `lexical-core` 浮点解析依赖。服务器拒绝进入 IDLE 时回到轮询。

- `vendor/imap-proto`：随上游同步 0.10.2 本地副本，保留 `nom` 的 std 功能与 IMAP BODYSTRUCTURE 兼容处理；兼容腾讯企业邮箱 CAPABILITY 末尾空格，同时校验 IMAP4rev1 和下一条响应边界。许可为 MIT/Apache-2.0，归属沿用第三方声明。

设置页线性图标沿用 Lucide，新增图标来自 [官方 icons 目录](https://github.com/lucide-icons/lucide/tree/main/icons)，仅将描边色设为白色供运行时 tint 使用；许可保留在 `assets/settings-icons/LICENSE`。

设置页内置 Noto Sans SC Regular 400；字体来源、实例化和 SHA-256 见 [字体说明](../assets/fonts/README.md)，采用 [SIL OFL 1.1](../assets/fonts/OFL-NotoSansSC.txt)。

## 安全检查

2026 年 11 月 3 日发布前，对 Cargo.lock 的 registry 依赖查询了 OSV 公告库，并运行完整测试。已移除受 [RUSTSEC-2023-0086](https://rustsec.org/advisories/RUSTSEC-2023-0086.html) 影响的 `lexical-core 0.7.6`。

当前仍有两项上游停止维护提示：

| 依赖 | 公告 | 当前处理 |
| --- | --- | --- |
| paste 1.0.15 | [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) | 随现有 UI 依赖使用，后续跟随上游升级 |
| ttf-parser 0.25.1 | [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html) | 随字体栈使用，后续评估维护中的替代版本 |

这两项属于维护状态提示，并非本次检查确认的可利用漏洞。公告库检查不能代替完整安全审计。`imap-proto` 还会产生 Rust 未来兼容性警告，当前编译和测试通过。
