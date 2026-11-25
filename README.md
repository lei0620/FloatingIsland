# 浮岛（FloatingIsland）

浮岛是一个悬浮在 Windows 桌面上的小工具，提供音乐与歌词、专注计时、喝水与久坐提醒、任务通知、剪贴板历史、邮箱及日程。常用设置可以通过文件或 WebDAV 同步到另一台电脑。

项目基于[岛仔](https://github.com/Renly1994/xiaotang-daozai)二次开发。浮岛原名 Leidao，已有安装入口和配置可继续使用。

## 下载安装

在[发布页](https://github.com/lei0620/FloatingIsland/releases/latest)下载 `FloatingIsland-v版本号-windows-x64.zip`，完整解压后运行 `Leidao.exe`。支持 64 位 Windows 10 和 Windows 11。

在「设置 → 关于 → 检查更新」可以直接检查并下载公开更新，无需 GitHub 账号或 GitHub CLI。原私有更新渠道的用户可以从旧版检查并升级到此版本。手动更新已有安装时，先退出程序，把 ZIP 中的两个 EXE 一起覆盖到原安装目录，沿用原数据目录和设置。

## 从源码运行

安装 Rust stable（MSVC）、Visual Studio C++ 生成工具和 Windows SDK，然后在仓库根目录运行：

```powershell
cargo build --locked --release --bin daozai
./target/release/daozai.exe
```

在设置里按需连接自己的邮件和 WebDAV 账号。WebDAV 可指定存储目录，连接时会尝试创建缺失的目录。WebDAV 应用密码由 Windows 凭据管理器保存。

## 许可

浮岛原创贡献可免费用于非商业用途，也可免费分享。未经权利人书面许可，禁止商用或收费销售包含这些原创贡献的版本。见[浮岛原创贡献非商业许可](LICENSE-FLOATING-ISLAND.md)。

岛仔代码保留[原版权声明和 MIT 许可](LICENSE-MIT.txt)。第三方组件、字体和素材另见[第三方声明](THIRD_PARTY_NOTICES.txt)。

## 联系与赞助

在「设置 → 关于 → 联系与赞助」可以复制维护者的公开微信号，也可以自愿通过支付宝支持项目。
