# 浮岛（FloatingIsland）

浮岛是一个悬浮在 Windows 桌面上的小工具，提供音乐与歌词、专注计时、喝水与久坐提醒、任务通知、剪贴板历史、邮箱及日程。常用设置可以通过文件或 WebDAV 同步到另一台电脑。

项目基于[岛仔](https://github.com/Renly1994/xiaotang-daozai)二次开发。浮岛原名 Leidao，已有安装入口和配置可继续使用。

## 从源码运行

安装 Rust stable（MSVC）、Visual Studio C++ 生成工具和 Windows SDK，然后在仓库根目录运行：

```powershell
cargo build --locked --release --bin daozai
./target/release/daozai.exe
```

在设置里按需连接自己的邮件和 WebDAV 账号。WebDAV 可指定存储目录，连接时会尝试创建缺失的目录。WebDAV 应用密码由 Windows 凭据管理器保存。本仓库提供源码，已有的程序更新渠道仍需相应仓库访问权限。

## 许可

浮岛原创贡献可免费用于非商业用途，也可免费分享。未经权利人书面许可，禁止商用或收费销售包含这些原创贡献的版本。见[浮岛原创贡献非商业许可](LICENSE-FLOATING-ISLAND.md)。

岛仔代码保留[原版权声明和 MIT 许可](LICENSE-MIT.txt)。第三方组件、字体和素材另见[第三方声明](THIRD_PARTY_NOTICES.txt)。

## 联系与赞助

在「设置 → 关于 → 联系与赞助」可以复制维护者的公开微信号，也可以自愿通过支付宝支持项目。
