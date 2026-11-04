# 分布式限流器

这是一个独立的、基于 Redis 的准入服务，实现了 Kong Gateway Take-home
Assignment 的选项 1。

多个无状态服务副本共享同一个权威 Redis 配额池。每个请求都会在一次原子的
Redis Lua 执行中，同时检查 Request Path、Client ID 和源 IP 三个 Token Bucket。

## 架构

```text
可信网关
   |
   +---- 限流器 1 ----\
   +---- 限流器 2 -----+---- Redis Primary
   +---- 限流器 3 ----/
```

- 限流器副本不保存权威配额状态。
- Redis Lua 以原子方式对三个 Bucket 进行补充、检查和条件扣减。
- Redis `TIME` 是唯一的补充时钟。
- 只有三个 Bucket 都允许请求时，才会分别扣除一个 Token。
- Redis 故障时返回 `503 Service Unavailable`，即 fail-closed。

一致性保证成立的前提是当前 Redis Primary 可用。详细权衡请参阅
[工程决策](docs/engineering-decisions.zh-CN.md)。

## 安装与运行

### Docker Compose（推荐）

系统要求：

- 安装了 Docker Engine 或 Docker Desktop，并提供 `docker compose` 命令
- 主机端口 `8081`–`8083` 空闲
- 主机端口 `6379` 空闲，因为开发用 Compose 文件会将 Redis 暴露到
  `127.0.0.1`，供主机侧测试和调试

启动一个 Redis Primary 和三个限流器副本：

```sh
docker compose up -d --build
docker compose ps
```

检查全部副本：

```sh
for port in 8081 8082 8083; do
  curl --fail "http://127.0.0.1:${port}/readyz"
done
```

三个实例分别位于 `http://127.0.0.1:8081`、`:8082` 和 `:8083`。

停止服务但保留 Redis 数据：

```sh
docker compose down
```

删除服务及全部持久化 Redis 状态：

```sh
docker compose down -v
```

### 平台支持

- 已在 Apple Silicon macOS 上使用 Docker Desktop 和原生二进制文件测试。
- 只要系统提供 `docker compose` 命令，Docker 部署预期可在 Linux 和 macOS
  上运行。
- 为目标操作系统和 CPU 架构构建后，原生二进制文件可在 macOS 或 Linux
  上运行；systemd 示例仅适用于 Linux。
- 尚未测试原生 Windows 和 PowerShell 工作流，Windows 环境推荐使用 WSL2。

### 主机开发（可选）

主机开发需要 Rust 1.85+、Python 3、Docker、Bash、`make` 和 `curl`。

```sh
make setup
```

`make setup` 会检查 Docker 和 Python；如果缺少 Cargo，则通过 `rustup`
安装 Rust；随后获取锁定版本的依赖、拉取 Redis 镜像并构建主机二进制文件。
它不会安装 Docker 或 Python。

使用 Docker 中的 Redis 在本机运行应用：

```sh
docker compose up -d redis
export REDIS_URL=redis://127.0.0.1:6379
cargo run
```

如需将原生二进制文件部署到多台机器，请参阅
[不使用 Docker 的多主机部署](docs/bare-metal-deployment.md)。

## 配置

### 进程配置

| 变量 | 默认值 | 用途 |
| --- | --- | --- |
| `BIND_ADDR` | `0.0.0.0:8080` | HTTP 监听地址 |
| `REDIS_URL` | `redis://127.0.0.1:6379` | 权威 Redis 端点 |
| `REDIS_OPERATION_TIMEOUT_MS` | `500` | Redis 操作超时 |
| `RUST_LOG` | `info` | 结构化日志过滤级别 |

所有副本必须连接同一个 Redis Primary。使用不同 Redis 实例会导致配额池被拆分。

### 限流策略

读取当前策略：

```sh
curl --fail http://127.0.0.1:8081/v1/config
```

通过任意健康副本替换策略：

```sh
curl -i -X PUT http://127.0.0.1:8081/v1/config \
  -H 'Content-Type: application/json' \
  -d '{
    "path":   { "limit": 100, "window_seconds": 60 },
    "client": { "limit": 50,  "window_seconds": 60 },
    "ip":     { "limit": 20,  "window_seconds": 10 }
  }'
```

三个维度都是必填项，并且始终启用：

- `limit`：Token Bucket 容量和最大突发量，范围为 `1` 至 `1,000,000`
- `window_seconds`：补充窗口，范围为 `1` 至 `86,400`
- 补充速率：`limit / window_seconds`
- 请求成本：每个维度扣除一个 Token

只有三个 Bucket 都拥有 Token 时，请求才会被允许。拒绝请求不会扣减任何
Token。每次成功更新策略都会递增 `generation`，并启用全新的 Bucket。

## API

### 健康检查

```http
GET /healthz
GET /readyz
```

`healthz` 检查进程状态；`readyz` 检查 Redis 和共享策略。

### 准入判断

```sh
curl -i -X POST http://127.0.0.1:8081/v1/admissions \
  -H 'Content-Type: application/json' \
  -d '{
    "request_id": "req-123",
    "path": "/payments/{payment_id}",
    "client_id": "merchant-123",
    "source_ip": "203.0.113.10"
  }'
```

| 状态码 | 含义 |
| --- | --- |
| `200` | 允许；从每个 Bucket 中扣除一个 Token |
| `400` | 请求无效 |
| `429` | 一个或多个维度没有可用配额 |
| `503` | Redis 无法提供可信的判断结果 |

`429` 响应包含 `Retry-After` 和所有造成限制的维度。Redis 故障时返回 `503`，
而不是不正确的 `200` 或 `429`。

网关会在转发业务请求前调用此 API；本服务不代理上游流量。

## 测试

```sh
make test-unit
make test-integration
make test-all
```

集成测试会启动三个共享同一个隔离 Redis 的限流器进程，并验证共享配额、
多维度原子扣减、策略 generation 重置、输入校验、`Retry-After` 和
fail-closed 行为。

## 流量模拟

向运行中的 Compose 副本发送单个请求：

```sh
curl -i -X POST http://127.0.0.1:8081/v1/admissions \
  -H 'Content-Type: application/json' \
  -d '{
    "request_id": "simulation-1",
    "path": "/simulation/path-0",
    "client_id": "simulation-client-0",
    "source_ip": "203.0.0.1"
  }'
```

使用端口 `8082` 或 `8083` 可访问其他副本。三个副本共享同一个 Redis 配额池。

向 Compose 集群发送突发流量：

```sh
python3 scripts/simulate_traffic.py \
  --base-urls http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083 \
  --pattern burst \
  --requests 100 \
  --concurrency 16
```

模拟器会输出状态码计数、各实例流量分布、吞吐量以及
p50/p95/p99/max 延迟。通过以下命令查看 `burst`、`steady` 和 `ramp` 选项：

```sh
python3 scripts/simulate_traffic.py --help
```

自动创建隔离 Redis 和三个主机进程：

```sh
make simulate PATTERN=burst REQUESTS=500 CONCURRENCY=64
```

`make test-integration` 和 `make simulate` 会使用主机端口 `6379` 和
`8081`–`8083`。运行它们之前，请先停止常规 Compose 集群。

## 技术文档

- [工程决策](docs/engineering-decisions.zh-CN.md)
- [原生多主机部署（英文）](docs/bare-metal-deployment.md)
- [English README](README.md)
