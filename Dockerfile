# lv-sandbox 多阶段构建
#   builder: rust:1-bookworm + libseccomp-dev 编译
#   runtime: debian:bookworm-slim + libseccomp2 + curl（非 root；curl 供 demo/排查用）
# 构建命令: docker build -t lv-sandbox:0.2.1 .

# ---- builder：编译阶段（需要 libseccomp-dev 头文件 + pkg-config）----
FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends libseccomp-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
# .dockerignore 已排除 target/ .git/ veps/ tests/ docs/ 等
COPY . .
# BuildKit cache mount 加速依赖重编译；cache 内容不进镜像层，故 cp 到 /usr/local/bin
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release -p sandbox-server -p sandbox-mcp && \
    cp /app/target/release/sandbox-server /app/target/release/sandbox-mcp /usr/local/bin/

# ---- runtime：运行阶段（仅需 libseccomp2 运行时）----
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends libseccomp2 ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
# 非 root 用户（uid 10000），与 docs/architecture.md 推荐部署对齐
RUN groupadd --gid 10000 sandbox && \
    useradd --uid 10000 --gid 10000 --create-home --shell /usr/sbin/nologin sandbox
COPY --from=builder /usr/local/bin/sandbox-server /usr/local/bin/sandbox-server
COPY --from=builder /usr/local/bin/sandbox-mcp  /usr/local/bin/sandbox-mcp
# 内置默认配置；server 缺 config 会启动失败，故必须落到 /etc/sandbox-server/config.yaml
COPY docker/config.yaml /etc/sandbox-server/config.yaml
RUN mkdir -p /sandboxes && chown -R 10000:10000 /sandboxes /etc/sandbox-server
USER 10000:10000
EXPOSE 8080
# server 支持 SIGTERM graceful shutdown（docker stop 正常退出）
ENTRYPOINT ["/usr/local/bin/sandbox-server"]
