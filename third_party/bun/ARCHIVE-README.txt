Bun 通知运行时对应源码

此归档与 open-richo-monitor 的试用安装包从同一网盘分享提供。
其中 Bun 本身采用 MIT，JavaScriptCore/WebKit 采用 LGPL-2，TinyCC
采用 LGPL-2.1；各组件源码保留其许可证与版权声明。

归档内容

bun-v1.4.2.tar.gz：Bun 官方 bun-v1.4.2 标签的完整源码与构建工程。
WebKit-bun-v1.4.2.tar.gz：上述 Bun 在 scripts/build/deps/webkit.ts
的 WEBKIT_VERSION 锁定的 WebKit 源码，保留 Source、Tools、根级
构建文件和许可证。此范围包含静态 JSC 所需的 JavaScriptCore、WTF、
bmalloc、WebCore、第三方源码及平台构建工具。
tinycc-bun-v1.4.2.tar.gz：scripts/build/deps/tinycc.ts 中
TINYCC_COMMIT 锁定的 TinyCC 源码。Bun 工程内保留对应补丁与编译参数。

重新构建

解压 Bun 源码，再将 WebKit 源码解压到 Bun 目录的 vendor/WebKit。
此 WebKit 树已经是 Bun 锁定的版本，快照无 Git 历史，跳过
bun sync-webkit-source。依照 Bun CONTRIBUTING.md 准备编译工具，
在 Bun 源码目录运行 bun run build:release:local，重新构建并链接
本地 JavaScriptCore。其他依赖的版本和下载方式由 Bun 构建工程指定。
TinyCC 归档可用于修改其库；Bun 的依赖描述与补丁在
scripts/build/deps/tinycc.ts 和 patches/tinycc 中。

Bun 构建所需 Git 元数据可通过检出官方 bun-v1.4.2 标签取得；本归档
提供对应机器可读源码。构建说明与原始仓库：
https://github.com/oven-sh/bun/blob/bun-v1.4.2/CONTRIBUTING.md
https://github.com/oven-sh/bun/tree/bun-v1.4.2
https://github.com/oven-sh/WebKit
https://github.com/oven-sh/tinycc

本应用使用独立 Bun 可执行文件运行 notification-runtime.mjs。
重建后可替换 Mac App 的 Contents/MacOS/notification-runtime，或
Windows 安装目录的 notification-runtime.exe。Mac 修改后须重新
签名，并保留运行时 JIT 权限；SOURCE.txt 随附相关说明。
