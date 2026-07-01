# lv-sandbox 多阶段构建
#   builder: rust:1-bookworm + libseccomp-dev 编译
#   runtime: debian:bookworm-slim + libseccomp2 + curl + python3/node（非 root；运行时供 agent 任务用）
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

# ---- runtime：运行阶段（Node.js 24 + libseccomp2 + python3）----
FROM debian:bookworm-slim AS runtime

# cr-049: 安装 Node.js 24(nodesource,替代 bookworm 默认 node 18)
RUN apt-get update && apt-get install -y --no-install-recommends curl gnupg && \
    curl -fsSL https://deb.nodesource.com/setup_24.x | bash - && \
    apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/*

# 系统依赖(libseccomp2 + python3 + curl + grep/sed/findutils;nodejs 已在上一层)
RUN apt-get update && apt-get install -y --no-install-recommends \
      libseccomp2 ca-certificates curl python3 python3-pip \
      grep sed gawk findutils \
 && rm -rf /var/lib/apt/lists/*

# cr-020 + cr-049: 数据科学栈(numpy pandas matplotlib scikit-learn)+基础库(requests httpx)
# 安装到 /usr/lib/python3/dist-packages(landlock 白名单内,不用改 landlock)
RUN python3 -m pip install --break-system-packages --target /usr/lib/python3/dist-packages --no-cache-dir \
    numpy pandas matplotlib scikit-learn requests httpx
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
