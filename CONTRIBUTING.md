# 贡献指南

## 开发依据

宿主与 SDK 固定官方 v3.34.0 提交 `47d30474af7c3e1fab433f2d4a70430fae548ff2`，UI 使用官方 v0.3.0 发行包
先阅读 [设计合同](docs/design.md)，周期判定与重置语义属于数据安全边界

使用 Rust 1.99.1、Node.js 24 和 pnpm 12.6.0

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend lint
pnpm --dir frontend test
pnpm --dir frontend build
```

联调必须在专用官方宿主及专用 PostgreSQL、Redis 中进行，使用合成账号与额度响应
通过真实 SDK、插件进程、安装器和管理页面验证，不以直接改库代替业务链路
响应丢失和进程退出可在传输边界定向注入，说明直接验证与推断范围

`cargo build --example fault-proxy` 构建仅用于隔离测试的传输代理，不进入发行包
代理读取 `/app/.runtime/data/follow-fault.json` 的 `binary` 和 `marker` 路径，启动被测插件
第一次收到真实宿主的 `host.keys.reset_budget` 回执时，创建一次性 marker 并中止进程，不将回执交给插件
随后确认插件恢复为“结果未知”，再次产生消费并检查其未被自动清零
在宿主使用前核对测试代理及被测二进制的平台和系统库兼容性；禁止在生产使用故障代理

## 提交与 PR

功能分支向 main 提 PR，检查通过后 squash 合并
提交使用英文 Conventional Commits，文档使用简体中文
PR 说明问题、行为、实际验证和未验证部分；页面变动附当前实现的合成数据截图
使用 AI 时披露工具和实际模型型号，未知时如实注明并保持 Draft
不得上传真实账号、令牌、生产地址和生产截图

页面金额仅格式化显示，不回写预算。页面与邮件时间固定为北京时间 UTC+8，只影响显示；跨日与偏移由展示测试覆盖，beta 升级保留状态仍需隔离宿主验证。

## 打包与发布

先构建插件二进制与页面，再从固定宿主提交构建 `codex-proxy-plugin-cli`

```sh
cargo build --release --locked
cpr-plugin package --manifest plugin.json --binary target/release/key-reset-follow \
  --target x86_64-unknown-linux-gnu --resource-map web=frontend/dist --output-dir dist
```

target 必须匹配实际编译平台，不通过修改平台参数伪装交叉编译
版本修改经 PR 合并，`v<version>` 标签与 plugin.json 逐字一致，只标记已验证提交
CI 改为手动触发，避免推送或标签消耗 Actions 额度。本地发布按 [本地发布](docs/local-release.md) 完成检查和两个架构的 tar.gz 及 SHA-256 文件；将真实本地检查结果回报为 `check` 提交状态后按 PR 合并。保留手动 CI 的原生双架构构建能力，带后缀的版本标为预发行。
已发布标签不移动，附件不覆盖；修正内容发布新版本
从预发行升级稳定版使用单独版本 PR，生产安装和稳定版发布须有对应用户授权
下载公开附件并核对摘要后才安装，安装后检查原有关联与状态保留
