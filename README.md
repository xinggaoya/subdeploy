# subdeploy (`sd`)

一个可迁移的 Rust 部署 CLI，用来把带 `Dockerfile` 和 Compose 文件的项目，通过 SSH 上传到远端服务器并完成构建、启动和可选健康检查。安装后的可执行文件名为 `sd`。

当前实现是一个独立小 workspace，后续可以整体迁移到别的仓库，不依赖 `cliproxy-rs` 现有业务 crate。

## 设计目标

- 使用 Rust 实现，方便后续 `cargo install` 到本机直接复用
- 按 `.gitignore` 语义收集部署文件，而不是硬编码当前仓库目录结构
- 通过 SSH 用户名 + 密码直接部署，不依赖 `sshpass`
- 只面向“单服务 Docker 项目”场景，部署前要求项目具备完整 Docker 部署文件

## 当前能力

- `validate`：校验项目是否满足部署前提
- `package`：生成部署归档，便于手工检查
- `deploy`：执行完整部署
- 省略 `deploy` 子命令时，默认直接执行部署

部署流程如下：

1. 本地扫描项目文件，遵循 `.gitignore` 规则
2. 校验 `Dockerfile` 和 Compose 文件存在
3. 本地打包为 `tar.gz`
4. 通过 SFTP 上传到远端 `/tmp`
5. 在远端创建 release 目录并切换 `current` 符号链接
6. 在远端执行 `docker build`
7. 在远端执行 `docker compose up -d`
8. 可选地在本地轮询健康检查地址

## 项目结构

```text
subdeploy/
├── apps/subdeploy/              # CLI 入口
├── crates/subdeploy-core/       # 部署编排、远端脚本、健康检查
├── crates/subdeploy-packager/   # 文件扫描、.gitignore 过滤、打包
├── crates/subdeploy-remote/     # SSH/SFTP 连接与远端执行
└── Cargo.toml                   # 独立 workspace
```

## 部署前提

目标项目必须满足下面条件：

- 项目根目录存在 `Dockerfile`
- 项目根目录存在 `docker-compose.yml` 或 `compose.yml`
- 远端机器已安装 `docker` 和 `docker compose`
- SSH 登录用户有权限执行 Docker 命令

当前版本不会自动生成 Docker 部署文件，也不会改写 Compose 内容。

## 安装

在 `subdeploy/` 目录内作为独立项目开发和测试：

```bash
cargo fmt --all --manifest-path subdeploy/Cargo.toml
cargo test --manifest-path subdeploy/Cargo.toml
```

安装到本机：

```bash
cargo install --path /path/to/subdeploy/apps/subdeploy
```

如果你当前就在本仓库目录：

```bash
cargo install --git https://github.com/xinggaoya/subdeploy
```

## 命令说明

查看帮助：

```bash
sd --help
sd deploy --help
```

### 1. 校验项目

```bash
sd validate --project-dir .
```

示例输出会包含：

- 项目目录
- 项目名
- Dockerfile 路径
- Compose 文件路径
- 归档文件数
- 默认远端目录

### 2. 生成归档

```bash
sd package \
  --project-dir . \
  --output ./build/deploy.tar.gz
```

这个命令适合先确认打包结果，再决定是否部署。

### 3. 执行部署

最小示例：

```bash
sd -u root -p 'your-password' -h 192.168.1.10
```

上面的默认部署等价于：

```bash
sd deploy \
  --project-dir . \
  --host 192.168.1.10 \
  --user root \
  --password 'your-password'
```

默认行为：

- `--image-tag` 不传时自动使用 `<project_name>:latest`
- `--compose-file` 不传时，优先使用 `docker-compose.yml`，没有时再回退到 `compose.yml`
- `--service` 不传时，若 compose 中只有一个服务则自动推断
- compose 中有多个服务时会报错，并提示你显式传入 `--service`
- 每次部署都会先停止旧服务、清空远端旧 release，再覆盖部署最新版本

带健康检查：

```bash
sd -u root -p 'your-password' -h 192.168.1.10 \
  --health-url http://192.168.1.10:8319/health \
  --health-timeout-secs 300 \
  --poll-interval-secs 5
```

显式指定部署文件：

```bash
sd deploy \
  --project-dir . \
  --dockerfile deploy/Dockerfile \
  --compose-file deploy/compose.yml \
  -h 192.168.1.10 \
  -u root \
  -p 'your-password' \
  --service app \
  --image-tag app:latest
```

## 参数说明

部署关键参数如下：

- `--project-dir`：项目根目录，默认当前目录
- `--dockerfile`：自定义 Dockerfile 路径，不传默认 `Dockerfile`
- `--compose-file`：自定义 Compose 文件路径，不传自动按 `docker-compose.yml`、`compose.yml` 顺序探测
- `-h, --host`：远端服务器地址
- `-P, --port`：SSH 端口，默认 `22`
- `-u, --user`：SSH 用户名
- `-p, --password`：SSH 密码
- `--service`：Compose 内服务名；不传时自动推断单服务 compose
- `--image-tag`：远端 `docker build -t` 的镜像名；不传时默认 `<project_name>:latest`
- `--remote-dir`：远端部署目录，默认 `/root/<project_name>-deploy`
- `--health-url`：可选健康检查地址
- `--health-timeout-secs`：健康检查超时秒数
- `--poll-interval-secs`：健康检查轮询间隔秒数
- `--no-cache`：远端构建时传递 `docker build --no-cache`
- `--help`：打印帮助；注意 `-h` 已用于 `--host`

## 当前仓库示例

对 `cliproxy-rs` 当前仓库，可以先做校验：

```bash
sd validate --project-dir /home/xinggao/dev/rust/cliproxy-rs
```

当前仓库的部署示例：

```bash
sd \
  --project-dir /home/xinggao/dev/rust/cliproxy-rs \
  -h sub.moncn.cn \
  -u root \
  -p 'your-password' \
  --health-url http://sub.moncn.cn:8319/health
```

## 打包规则说明

- 使用 `.gitignore` 规则过滤文件
- 不会把 `.git/` 打进归档
- 未被 `.gitignore` 忽略的点文件会保留，例如 `.dockerignore`、`.env.example`
- `Dockerfile` 和 Compose 文件虽然会参与校验，但如果它们被 `.gitignore` 忽略，部署会直接失败

这意味着部署文件必须是真正纳入项目源码管理的文件。

## 远端目录约定

默认远端目录格式：

```text
/root/<project_name>-deploy
```

内部结构如下：

```text
<remote-dir>/
├── current -> releases/<release-id>
└── releases/
    ├── <release-id-1>/
    └── <release-id-2>/
```

部署时会先尝试在旧 `current` 目录下执行一次 `docker compose down`，然后清空远端旧 releases，只保留本次部署的新 release，再启动服务。这样重复部署时不会被上一次残留文件阻塞，但当前版本仍不是零停机部署。

## 已知限制

- 仅支持 SSH 用户名 + 密码，不支持私钥认证
- 仅支持单服务 Docker 项目，不支持多服务编排策略定制
- 不支持回滚命令
- 不支持前端静态资源发布
- 不支持自动补齐环境变量或配置文件
- 默认假设 Compose 文件本身已经能正确引用目标镜像和挂载配置

## 开发说明

常用命令：

```bash
cargo fmt --all --manifest-path subdeploy/Cargo.toml
cargo test --manifest-path subdeploy/Cargo.toml
cargo run --manifest-path subdeploy/Cargo.toml -p sd -- --help
```

清理构建产物：

```bash
cargo clean --manifest-path subdeploy/Cargo.toml
```

`subdeploy/.gitignore` 已忽略 `target/`，便于在当前仓库内开发但不污染仓库状态。
