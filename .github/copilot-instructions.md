下面是为 AI 编码代理量身定制的快速上手说明，帮助你在 `rbph-website-backend` 仓库中快速、安全且一致地修改代码。

**仓库概览**
- **类型**: Rust 后端服务，基于 `actix-web`（见 `src/main.rs`）。
- **运行时依赖**: PostgreSQL（SQLx）和 Redis（session + deadpool-redis）。相关配置在 `Cargo.toml`。
- **主要目录**:
  - `src/api/` ：HTTP 路由与 handler，按域拆分（例如 `auth.rs`, `game.rs`, `user.rs`）。
  - `src/db/` ：与数据库交互的模块（例如 `db::create_pool` 在 `src/db/mod.rs`）。
  - `src/model/` ：领域模型（例如 `src/model/user.rs` 定义的 `RbUserRole`）。
  - `src/middleware/` ：中间件（例如权限中间件 `privilege.rs`，与 `RbUserRole` 联动）。
  - `src/extractor/`：请求 extractors（认证相关）。
  - `migrations/`：SQL migration 脚本（`001_create_tables.sql`、`002_insert_test_data.sql`）。

**关键架构与数据流（简练）**
- HTTP 请求进入 `actix-web`，由 `src/api/mod.rs` 的 `web::scope` 分发到子模块（例：`/api/auth` -> `src/api/auth.rs`）。
- 权限通过 `PrivilegeMiddleware`（`src/middleware/privilege.rs`）驱动，权限等级由 `RbUserRole`（`src/model/user.rs`）定义。
- 数据访问集中在 `src/db/*`，handler 调用这些模块进行 SQL 操作。
- 会话存放在 Redis：`SessionMiddleware` 使用 `RedisSessionStore`（初始化见 `src/main.rs`），cookie 名称为 `rbph_session`。

**配置与运行（必要命令示例，PowerShell）**
- 使用 `config.toml`（仓库根有示例）。可用环境变量覆盖，前缀为 `RBPH`，分隔符 `__`，例如：
  - PowerShell 设置 DB 地址：
    ```powershell
    $env:RBPH__APP__DB_ADDR = "postgres://postgres:123456@localhost/rbph"
    $env:RBPH__APP__KV_ADDR = "redis://localhost/1"
    $env:RUST_LOG = "info"
    ```
- 运行：
  ```powershell
  cargo build --release
  cargo run --release
  ```
- 注意：`src/config.rs` 要求 `app.secret_key` 为 base64 字符串（会被解码并填充到 64 字节）。示例见根目录 `config.toml`。

**项目配置（config）**
- 主配置位于 `src/config.rs`，核心类型与行为：
  - `AppConfig`:
    - `production: bool` : 是否为生产模式（影响 cookie secure、日志等）。
    - `bind_addr: (String, u16)` : 绑定地址与端口（示例：`["127.0.0.1", 9999]` 在 `config.toml`）。
    - `db_addr: String` : PostgreSQL 连接字符串（`RBPH__APP__DB_ADDR` 可覆盖）。
    - `kv_addr: String` : Redis 连接字符串（`RBPH__APP__KV_ADDR` 可覆盖）。
    - `secret_key: String` : base64 编码的 session secret（由 `get_secret_key()` 解码并填充到 64 字节）。
  - `AuthConfig`:
    - `max_session: usize` : 单用户最大会话数（示例在 `config.toml`）。
    - `captcha: CaptchaConfig` : 占位 struct（目前为空，但保留层次）。
  - `Settings`:
    - 顶级组合，包含 `app: AppConfig` 和 `auth: AuthConfig`。

- 加载与覆盖规则：
  - `Settings::read_from_file(file)` 使用 `config::File::with_name(file).required(true)` 加上 `config::Environment::with_prefix("RBPH").separator("__")`，因此你可以通过环境变量覆盖配置项。例如：
    - `RBPH__APP__DB_ADDR` 覆盖 `app.db_addr`。
    - `RBPH__APP__PRODUCTION=true` 覆盖 `app.production`。
  - 示例（PowerShell）：
    ```powershell
    $env:RBPH__APP__DB_ADDR = "postgres://postgres:123456@localhost/rbph"
    $env:RBPH__APP__KV_ADDR = "redis://localhost/1"
    $env:RBPH__APP__PRODUCTION = "false"
    ```

- `get_secret_key()` 行为说明：
  - 将 `app.secret_key` 用 Base64 解码（使用 `base64::prelude::BASE64_STANDARD`）。
  - 如果解码失败，会记录 warn 并使用 64 个零字节的默认 key。若成功，解码结果会被复制（或截断）到 64 字节数组中返回。

- 在做与会话或 cookie 有关的改动时，请参考 `src/main.rs` 中 `SessionMiddleware::builder(...).cookie_secure(config.production)` 的使用，确保 `app.production` 与 cookie 安全选项一致。

**数据库与迁移**
- 仓库含 `migrations/` SQL 文件。当前 `create_pool` 已在启动时执行嵌入的 `sqlx` 迁移（`sqlx::migrate!().run(&pool).await`），因此在默认启动时会尝试自动应用迁移并在失败时阻止服务启动（见 `src/db/mod.rs`）。
- 本地也可以手动应用 SQL：
  ```powershell
  psql -U postgres -d rbph -f migrations/001_create_tables.sql
  psql -U postgres -d rbph -f migrations/002_insert_test_data.sql
  ```
 - 迁移目录说明：
   - `migrations/001_create_tables.sql`：创建核心表（用户 `rb_user`、队伍 `rb_team`、队伍成员 `rb_team_member`、比赛 `rb_game`、公告 `rb_anmt`、谜题等表）。在添加模型或更改表结构时应修改/新增 migration 文件。
   - `migrations/002_insert_test_data.sql`：插入开发/测试用的示例数据，便于本地调试接口。
 - 添加新迁移建议：
   - 本项目既支持编写纯 SQL 文件并把它们放入 `migrations/`，也可以使用 `sqlx-cli`（可选）管理迁移。若使用 `sqlx-cli`，请在 CI/本地确保 `DATABASE_URL`/`RBPH__APP__DB_ADDR` 被正确设置，然后运行 `sqlx migrate add <name>` / `sqlx migrate run`。
   - 手工方式：创建新的 `migrations/00X_description.sql` 并在 CI 或本地通过 `psql` 或运行时的嵌入迁移应用它们。
 - 在 CI 中：目前 workflow 会通过 `psql` 逐个应用 `migrations/*.sql`（见 `.github/workflows/ci.yml` 的 `Apply SQL migrations` 步骤），确保测试执行前模式已就绪。

**CI 与 Docker（快速示例）**
- 已添加：`Dockerfile`（root）和 `docker-compose.yml`（root）用于本地开发：
  - 启动依赖服务并运行应用：
    ```powershell
    docker-compose up --build
    ```
  - 生产镜像构建：
    ```powershell
    docker build -t rbph-backend .
    docker run -e RBPH__APP__DB_ADDR="postgres://..." -e RBPH__APP__KV_ADDR="redis://..." -p 9999:9999 rbph-backend
    ```
- GitHub Actions CI workflow（`.github/workflows/ci.yml`）会在 `push`/`pull_request` 到 `main` 时构建并运行 `cargo test`，并使用临时的 Postgres/Redis 服务。

**代码示例（handler 与 DB 查询）**
- Handler 使用 `web::Data` 注入共享状态（`DbPool`/`KvPool`/`Settings`），并遵循 `api` 子模块的 `config(cfg: &mut ServiceConfig)` 约定。
  - 示例 handler（`src/api/user.rs` 风格）：
    ```rust
    use actix_web::{web, HttpResponse, Result};
    use crate::DbPool;

    pub async fn get_user_display(db: web::Data<DbPool>, path: web::Path<i32>) -> Result<HttpResponse> {
        let user_id = path.into_inner();
        let data = crate::db::user::get_display_by_id(&db, user_id).await.map_err(|e| {
            actix_web::error::ErrorInternalServerError(e)
        })?;
        Ok(HttpResponse::Ok().json(data))
    }
    ```
- DB 查询示例（参考 `src/db/user.rs`）使用 `sqlx` 的宏：
  - `sqlx::query_scalar!` 用于单列返回（示例：`register` 返回 `id`）。
  - `sqlx::query_as!` 用于将行映射到 struct（示例：`get_display_by_id` 返回 `RbUserDisplayData`）。

如果你想把 CI 扩展为在 job 中应用迁移或在容器启动时运行额外初始化（seed、feature flags），我可以把示例添加到 workflow 或 `entrypoint` 脚本里。

**API 模块职责与实现模式（快速参考）**
- `src/api/auth.rs` — 认证/会话逻辑
  - 实现点：`login` / `register` / `verify` / `logout`。输入校验使用 `Regex`（见 `EMAIL_REGEX` / `PWD_REGEX`），密码使用 `bcrypt` 验证/哈希。
  - 会话管理：调用 `module::session` 中的 helpers（例：`session::append`, `session::invalidate`），并使用 `actix-session::Session` 对象；登录后调用 `sess.renew()`。
  - 注册流程：先用 `db::user::put_pending` 将用户信息（带哈希密码）存入 Redis（key `pending_user:{token}`），然后通过 `/verify?token=...` 调用 `db::user::verify_pending` 完成入库。
  - 返回值模式：使用数字 `code` enum 与 `serde_repr` 序列化（例如 `UserLoginResult`, `UserRegisterResult`），便于前端统一处理错误码。

- `src/api/user.rs` — 用户资料与个人端点
  - 实现点：受保护的用户信息接口，使用 `extractor::auth::AuthUser` 来取得已认证用户（`user.uid`），避免在 handler 中重复解析 session。
  - 示例：`info` handler 调用 `db::user::get_display_by_id(&db_pool, user.uid)` 并返回 `RbUserDisplayData`。
  - 约定：在 `src/api/mod.rs` 中将 `/user` scope 包裹 `PrivilegeMiddleware::new(RbUserRole::User)`，使普通用户才能访问。

- `src/api/team.rs` — 队伍生命周期与权限
  - 实现点：创建/更新/加入/离开队伍（`create_self`, `join`, `leave_self` 等）。常见校验包括密码格式、队伍状态（`RbTeamState`）、成员上限与并发 TOCTOU 注意。
  - 数据流：多数操作通过 `db::team` 模块完成（例如 `user_create`, `join`, `leave`, `get_by_id_verify`），并对返回值做 `Option`/`bool` 判定后映射到 HTTP 错误码或业务 code。
  - 权限：使用 `AuthUser` 判断发起者身份，必要时在 `db` 层对用户权限/归属进行进一步校验。

  - `src/api/game.rs` — 比赛与公开信息
    - 实现点：比赛信息查询（`get_info`）、在线列表（`list_online`）、公告（`get_anmts`）等。
    - 中间件模式：使用 `check_game_id_middleware` 在路由级别检查 `game_id` 的合法性并基于 `RbUserRole` 做可见性判断，避免在每个 handler 内重复校验。
    - 嵌套路由：`/games/{game_id}` 下包裹权限受限的子 scope（例如 `/teams`），通过 `PrivilegeMiddleware` 控制访问边界（参考 `config` 函数）。

  - `src/api/admin/game.rs` — 管理员端比赛配置接口
    - 实现点：管理员可以创建（`append`）或编辑比赛配置（`edit`）。这些路由通常被 `src/api/mod.rs` 的 `/admin` scope 包裹并由 `PrivilegeMiddleware::new(RbUserRole::Admin)` 保护。
    - 管理端 handler 通常较薄：验证输入 -> 调用 `db::game` 的写操作 -> 返回标准化的 `code` 或 `204/200`。

  **实现提示（针对 AI 代理）**
  - 对于 `game` 模块的路由：优先复用 `db::game` 的 `exists`, `get_by_id`, `list_all` 等 helpers；如果需要限制可见性，利用 `RbUserRole` 在 middleware 层过滤。
  - 管理端改动（`admin/game.rs`）：做变更时确保操作在事务中（`sqlx::query!` + `pool.begin().await?`）并在成功后写入公告或触发缓存刷新（如需要）。

  **数据访问层（`src/db/*`）**
  - 概览：`src/db` 下每个模块封装与表对应的 SQL 操作（`user.rs`, `team.rs`, `game.rs`, `anmt.rs`, `puzzle.rs`）。Handler 应优先调用这些 helpers。
  - 常见模式：
    - 使用 `sqlx::query_scalar!` 获取单列值（如 `INSERT ... RETURNING id` 的 `register` / `append`）。
    - 使用 `sqlx::query_as!` 或 `QueryBuilder::build_query_as::<T>()` 将行映射到 struct（见 `game::RbGameShowData`, `team::RbTeamFullData`, `anmt::RbAnmt`）。
    - 可选行使用 `fetch_optional(pool).await?` 并返回 `Option<T>`；单行期待使用 `fetch_one(pool).await?`。
    - 动态条件或可变 WHERE 子句使用 `sqlx::QueryBuilder`（示例：`game::exists`, `game::list_all`, `anmt::list_all`）。
    - 当需在多个表间保持原子性时使用事务：`let mut tx = pool.begin().await?; ... tx.commit().await?;`（示例：`team::user_create`）。
    - 对于插入/更新后的影响行数判断使用 `result.rows_affected() > 0`（示例：`team::join` / `team::leave`）。

  - 错误与返回约定：
    - DB 模块函数一般返回 `Result<T, RbInternalError>`（参考 `error.rs`）；当 `sqlx::Error::RowNotFound` 需特殊处理时，模块会将其映射为 `Ok(None)`（示例：`anmt::get`）。
    - 业务层（handler）通常将 `Option<T>` 映射为 HTTP 404（`RbError::not_found()`）。

  - 命名约定与行为示例：
    - `append` / `register`：插入并 `RETURNING id`（返回 `i32`）。
    - `get_by_id`, `get_by_user_game`：读取单条或聚合数据，返回 `Option<T>`。
    - `list_all`：分页未实现的情况下返回全部结果（注意可能需要后续分页优化）。

  - 未实现/注意点：
    - `src/db/puzzle.rs` 当前为空 — 如果添加谜题相关表，请复用 `query_as!`/`query_scalar!` 模式并考虑缓存策略。
    - 若打算增加缓存（Redis），优先在 `db` 层做透明缓存封装并保证读写一致性策略（例如在写操作后失效缓存）。

  **快速参考（代码片段）**
  - 单行插入并返回 id：
    ```rust
    let id = sqlx::query_scalar!(
      "INSERT INTO rb_game (title, start_at, end_at) VALUES ($1,$2,$3) RETURNING id;",
      data.title, data.start_at, data.end_at
    ).fetch_one(pool).await?;
    ```
  - 查询并映射到 struct：
    ```rust
    let game: Option<RbGameShowData> = sqlx::query_as!(
      RbGameShowData,
      "SELECT id, title, start_at, end_at FROM rb_game WHERE id = $1;",
      game_id
    ).fetch_optional(pool).await?;
    ```

  引用文件：`src/db/user.rs`, `src/db/team.rs`, `src/db/game.rs`, `src/db/anmt.rs`。

  **模型（`src/model/*`）说明**
  - 概览：`src/model` 定义了与数据库表直接对应的结构体（多数带 `FromRow` / `Serialize`），以及用于业务逻辑的 enum（使用 `num_enum` + `serde`）。
  - `RbUser`（`src/model/user.rs`）关键字段示例：
    - `id: i32`, `email: String`, `pass: String`, `urole: RbUserRole`, `nickname: String`, `bio: Option<String>`, `ctime_at: OffsetDateTime`。
    - 权限用 `RbUserRole` enum 表示（`num_enum::FromPrimitive/IntoPrimitive`），并实现了便捷方法 `is_admin()` / `is_active()`。
  - `RbGame` / `RbTeam`（`src/model/game.rs`）示例：
    - `RbGame` 包含 `id, title, is_shown, is_online, reg_open_at, pre_open_at, start_at, end_at, ctime_at, cover`，并有 `is_started()` / `is_ended()` 辅助方法。
    - `RbTeam` 与 `RbTeamState` 描述队伍状态与关键信息（`tname, tstate, pass, bio, game_id, ctime_at`）。
  - 公告模型 `RbAnmt`（`src/model/anmt.rs`）：`id, title, content, is_pinned, is_shown, game_id, ctime_at`。
  - `puzzle` 模型：`src/model/puzzle.rs` 目前为空 —— 推荐字段（示例实现）：
    ```rust
    use serde::{Deserialize, Serialize};
    use sqlx::prelude::FromRow;
    use sqlx::types::time::OffsetDateTime;

    #[derive(FromRow, Serialize, Deserialize)]
    pub struct RbPuzzle {
        pub id: i32,
        pub title: String,
        pub content: String,
        pub answer: String,
        pub hint: Option<String>,
        pub game_id: i32,
        pub ctime_at: OffsetDateTime,
    }
    ```
    - 说明：如果你添加 `RbPuzzle`，优先使用 `query_as!` 将查询映射到该 struct。

  - 约定与注意事项：
    - 模型字段名应与 DB 列名匹配（`sqlx::query_as!` 依赖编译时校验）。
    - 对于枚举/整数映射，使用 `num_enum` 与 `serde` 的组合来保证可序列化与可比对。
    - 在模型中保留 `OffsetDateTime`（`sqlx` 的时间类型）以兼容现有查询。

  **基础架构 / 工具（middleware / extractor / error）**
  - `src/middleware/`：实现接口的中间件（当前仅有 `privilege.rs`）。
    - `PrivilegeMiddleware` 行为：
      - 在 `new_transform` 中捕获 `DbPool` 与 `KvPool`（通过 `web::Data`）。
      - 调用 `module::session::verify(&kv_pool, &sess).await` 校验会话；若失败会 `sess.purge()` 并返回 `RbError::unauth()`。
      - 若会话有效，从 `Session` 获取 `user_id` 并调用 `db::user::get_role_by_id` 获取角色。
      - 将 `RbUserRole` 插入到请求 `extensions` 中（`req.extensions_mut().insert(role)`），并在权限不足时返回 `RbError::forbid()`。
      - 中间件把错误统一用 `RbError` / `RbInternalError` 返回（见 `src/error.rs`）。

  - `src/extractor/`：自定义请求参数提取器（目前实现 `auth::AuthUser`）。
    - `AuthUser` 从 `Session` 读取 `user_id`，并从请求 `extensions` 取出 `RbUserRole`（`Banned` 为默认）。
    - 如果 session 无 `user_id`，提取器会 `sess.purge()` 并返回 `RbError::unauth()` 错误（作为 `FromRequest` 的 Err 返回）。

  - `src/error.rs`：统一错误处理逻辑
    - `RbError`：外部可见的 HTTP 错误结构，包含 `code: i32`, `message: Option<String>` 与 `status_code: StatusCode`，并实现 `ResponseError`。常用构造函数：`unauth()`, `forbid()`, `not_found()`, `internal()` 等；使用 `resp()` 生成 `HttpResponse`。
    - `RbInternalError`：内部错误枚举（`Sql`, `Bcrypt`, `Redis`, `Json`, `Session` 等），也实现 `ResponseError` 并将内部错误映射为 `RbError::internal(...)` 返回给客户端（不泄露内部细节，但记录随机错误代码到日志）。
    - 约定：handler 或 db 层返回 `Result<..., RbInternalError>`，handler 在需要时将其转为 HTTP 响应；业务错误优先使用 `RbError` 来表示可序列化的 API 错误码。

  **实现提示（针对 AI 代理）**
  - 在实现中间件时，优先从 `web::Data` 获取池并在缺失时返回 `RbError::internal("... pool not found")`（参考 `privilege.rs`）。
  - 使用 `req.extensions()` 作为跨中间件传递上下文（例如 `RbUserRole`），而不是在每个 handler 中重复查询数据库。
  - 错误返回：handler 可以直接用 `RbError::...` 链式构造并 `err()?` 导出错误，或将 `RbInternalError` 直接返回到 actix，框架会把它映射为 `RbError::internal`。


**实现提示（针对 AI 代理）**
- 当实现新 handler：先查看 `src/api/mod.rs` 的 scope/中间件配置，遵循同样的 `default_service` 错误处理模式。
- 使用 repo 中已有 DB helpers（`src/db/user.rs`, `src/db/team.rs`）优先复用，不要直接写原始 SQL 除非新增表或功能。
- 对于会话与并发：参考 `module/session.rs`（session helpers）与 `actix-session` 的 `Session` 用法，登录后记得 `sess.renew()`，登出时 `sess.purge()`。
- 返回结构采用 `{ code: i32, ... }` 的模式，code 使用 `repr` enum 保持前后端一致。

---
请审阅这些补充：告知是否需要把 `module/session.rs`、`extractor/auth.rs` 的实现细节也加入指南，或把示例扩展为可运行的代码片段。

**编码/改动约定（在此仓库内要遵守的可观测规则）**
- 路由注册：优先在 `src/api/mod.rs` 做 `web::scope` 的组合与权限包装，子模块只负责 `config(cfg: &mut ServiceConfig)`。
- 共享状态：通过 `web::Data` 传递数据库连接池（`DbPool`）、Redis pool、以及 `Settings`；在 `HttpServer::new` 闭包中 `.clone()` 传递。
- 权限：使用 `RbUserRole` 做比较；中间件会以该 enum 为边界，避免在 handler 内重复校验。
- 错误处理：`api::config` 使用 `default_service` 返回统一 forbidden 错误，新增路由应保持同样的默认服务模式。

**调试与日志**
- 使用 `env_logger`，通过 `RUST_LOG` 控制日志级别（例如 `debug,rbph=info`）。在 PowerShell 中：
  ```powershell
  $env:RUST_LOG = "debug"
  cargo run
  ```

**集成点 & 依赖注意事项**
- PostgreSQL：通过 `sqlx`（`features = ["postgres"]`）访问；连接字符串在 `config.toml` 或 `RBPH__APP__DB_ADDR`。
- Redis：`deadpool-redis` 池与 `actix-session` 的 `RedisSessionStore`。Redis 地址在 `config.toml` 的 `app.kv_addr`。

如果有任何关于运行环境、CI、或想要我把 `sqlx` 迁移改为自动执行的偏好，请告诉我 — 我会据此更新说明或修改启动逻辑。

---
请审阅以上内容：指出需要补充的具体区域（例如 CI、Docker、或更细的编码规范），我会据此迭代此文件。
