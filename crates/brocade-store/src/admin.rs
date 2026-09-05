use argon2::{
    password_hash::{
        Error as PasswordHashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::input::required_text;
use crate::{
    credentials::{
        admin_session_token_hash, admin_token_display_prefix, admin_token_hash,
        generate_admin_password, generate_admin_session_token, generate_admin_token,
    },
    Result, StoreError,
};

pub const ADMIN_SESSION_TTL_SECONDS: i32 = 12 * 60 * 60;
const ADMIN_LAST_USED_TOUCH_SECONDS: i32 = 60;
const MIN_ADMIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdminRole {
    User,
    Readonly,
    Editor,
    Publisher,
    TenantAdmin,
    SystemAdmin,
}

impl AdminRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AdminRole::User => "user",
            AdminRole::Readonly => "readonly",
            AdminRole::Editor => "editor",
            AdminRole::Publisher => "publisher",
            AdminRole::TenantAdmin => "tenant-admin",
            AdminRole::SystemAdmin => "system-admin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAdminOperatorRequest {
    pub id: String,
    pub display_name: String,
    pub role: AdminRole,
    pub tenant_scope: Option<String>,
    // Passwordless access is reserved for the fixed `public` readonly account. For an existing
    // password-protected operator, omitting this field preserves the current password.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminInitRequest {
    pub operator_id: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLoginRequest {
    pub operator_id: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminOperator {
    pub id: String,
    pub display_name: String,
    pub role: AdminRole,
    pub tenant_scope: Option<String>,
    // Only the fixed `public` readonly account may be passwordless.
    pub passwordless: bool,
    pub token_prefix: Option<String>,
    pub token_created_at: Option<String>,
    pub token_last_used_at: Option<String>,
    pub token_revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedAdminToken {
    pub operator_id: String,
    pub token: String,
    pub token_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetAdminPasswordResult {
    pub operator_id: String,
    pub password: String,
    pub sessions_revoked: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeAdminPasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetUserPasswordRequest {
    pub new_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetUserPasswordResult {
    pub operator_id: String,
    pub sessions_revoked: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedAdminSession {
    pub token: String,
    pub expires_at: String,
}

/// The operator whose existence opens the console to anybody who loads the page.
///
/// It remains a fixed internal login identity, while the product exposes it as a simple visitor
/// switch rather than general operator management.
pub const PUBLIC_OPERATOR_ID: &str = "public";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAuthState {
    pub initialized: bool,
    /// Whether the fixed passwordless visitor identity exists.
    pub public_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminInitResult {
    pub admin: AuthenticatedAdmin,
    pub session: IssuedAdminSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLoginResult {
    pub admin: AuthenticatedAdmin,
    pub session: IssuedAdminSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedAdmin {
    pub operator_id: String,
    pub role: AdminRole,
    pub tenant_scope: Option<String>,
    pub token_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_user: Option<AuthenticatedUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedUser {
    pub tenant_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedUserLogin {
    pub operator_id: String,
    pub password: String,
    pub sessions_revoked: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminContext {
    operator_id: String,
    role: AdminRole,
    tenant_scope: Option<String>,
    self_user: Option<AuthenticatedUser>,
}

impl AdminContext {
    pub fn new(
        operator_id: impl Into<String>,
        role: AdminRole,
        tenant_scope: Option<String>,
    ) -> Self {
        Self {
            operator_id: operator_id.into(),
            role,
            tenant_scope: tenant_scope
                .and_then(|scope| normalize_optional_text(&scope).map(str::to_owned)),
            self_user: None,
        }
    }

    pub fn system_admin(operator_id: impl Into<String>) -> Self {
        Self {
            operator_id: operator_id.into(),
            role: AdminRole::SystemAdmin,
            tenant_scope: None,
            self_user: None,
        }
    }

    pub fn from_authenticated(admin: &AuthenticatedAdmin) -> Self {
        let mut actor = Self::new(
            admin.operator_id.clone(),
            admin.role,
            admin.tenant_scope.clone(),
        );
        actor.self_user = admin.self_user.clone();
        actor
    }

    pub fn operator_id(&self) -> &str {
        &self.operator_id
    }

    pub fn role(&self) -> AdminRole {
        self.role
    }

    pub fn tenant_scope(&self) -> Option<&str> {
        self.tenant_scope.as_deref()
    }

    pub fn self_user(&self) -> Option<&AuthenticatedUser> {
        self.self_user.as_ref()
    }

    pub fn is_system_admin(&self) -> bool {
        self.role == AdminRole::SystemAdmin
    }

    /// Whether the visibility is global — no tenant_scope means not narrowed to one branch.
    ///
    /// It gates the read-only views organized as one global network that cannot be split along
    /// tenant lines: backbone MTU and per-hop liveness are properties of the backbone, and a
    /// link's two ends can belong to two different tenants, so they cannot be scoped apart.
    /// `is_system_admin` is the wrong test for this gate: it excludes global read-only roles
    /// that by their nature should see the whole network, while admitting tenant-admins, who
    /// are precisely the ones that should be scoped.
    pub fn is_global_scope(&self) -> bool {
        self.tenant_scope.is_none()
    }

    pub fn can_access_tenant(&self, tenant_id: &str) -> bool {
        self.is_system_admin()
            || self
                .tenant_scope()
                .is_some_and(|scope| tenant_in_scope(tenant_id, scope))
    }

    pub fn tenant_scope_like_pattern(&self) -> Option<String> {
        self.tenant_scope().map(tenant_scope_like_pattern)
    }

    pub fn require_tenant_access(&self, tenant_id: &str, action: &str) -> Result<()> {
        if self.can_access_tenant(tenant_id) {
            Ok(())
        } else {
            Err(StoreError::Forbidden(format!(
                "{action} is outside tenant scope"
            )))
        }
    }
}

pub fn tenant_in_scope(tenant_id: &str, scope: &str) -> bool {
    tenant_id == scope
        || tenant_id
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub fn tenant_scope_like_pattern(scope: &str) -> String {
    let mut pattern = String::with_capacity(scope.len() + 3);
    for ch in scope.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push_str(".%");
    pattern
}

pub async fn admin_auth_state(pool: &PgPool) -> Result<AdminAuthState> {
    let row = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM admin_operators) AS initialized,
                EXISTS (
                    SELECT 1 FROM admin_operators
                    WHERE id = $1 AND password_hash IS NULL
                ) AS public_open",
    )
    .bind(PUBLIC_OPERATOR_ID)
    .fetch_one(pool)
    .await?;
    Ok(AdminAuthState {
        initialized: row.try_get("initialized")?,
        public_open: row.try_get("public_open")?,
    })
}

/// Turn the fixed visitor identity on or off without exposing general operator management.
///
/// Disabling deletes the row so its browser sessions disappear through the session foreign key.
/// Enabling recreates it as a passwordless readonly identity scoped to the installation's root
/// tenant. User login identities are separate and are not affected.
pub async fn set_public_access(
    pool: &PgPool,
    actor: &AdminContext,
    enabled: bool,
) -> Result<AdminAuthState> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can change visitor access".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;
    if enabled {
        let existing_scope: Option<Option<String>> =
            sqlx::query_scalar("SELECT tenant_scope FROM admin_operators WHERE id = $1 FOR UPDATE")
                .bind(PUBLIC_OPERATOR_ID)
                .fetch_optional(&mut *tx)
                .await?;
        let tenant_scope = match existing_scope.flatten() {
            Some(scope) => scope,
            None => sqlx::query_scalar::<_, String>(
                "SELECT id
                   FROM tenants
                  ORDER BY array_length(string_to_array(id, '.'), 1), id
                  LIMIT 1",
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                StoreError::InvalidData(
                    "create the root tenant before enabling visitor access".to_owned(),
                )
            })?,
        };
        sqlx::query(
            "INSERT INTO admin_operators (
                id, display_name, role, tenant_scope, password_hash,
                token_hash, token_prefix, token_created_at, token_last_used_at, token_revoked_at,
                user_tenant_id, user_id
             ) VALUES ($1, '访客', 'readonly', $2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)
             ON CONFLICT (id) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                role = EXCLUDED.role,
                tenant_scope = EXCLUDED.tenant_scope,
                password_hash = NULL,
                token_hash = NULL,
                token_prefix = NULL,
                token_created_at = NULL,
                token_last_used_at = NULL,
                token_revoked_at = NULL,
                user_tenant_id = NULL,
                user_id = NULL",
        )
        .bind(PUBLIC_OPERATOR_ID)
        .bind(tenant_scope)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("DELETE FROM admin_operators WHERE id = $1")
            .bind(PUBLIC_OPERATOR_ID)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    admin_auth_state(pool).await
}

pub async fn init_admin(pool: &PgPool, request: AdminInitRequest) -> Result<AdminInitResult> {
    let operator_id = required_text(&request.operator_id, "admin operator id")?;
    let display_name = required_text(&request.display_name, "admin operator display_name")?;
    let password_hash = hash_admin_password(&request.password)?;

    let mut tx = pool.begin().await?;
    sqlx::query("LOCK TABLE admin_operators IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let existing: i64 = sqlx::query("SELECT count(*) AS n FROM admin_operators")
        .fetch_one(&mut *tx)
        .await?
        .try_get("n")?;
    if existing != 0 {
        return Err(StoreError::Forbidden(
            "admin has already been initialized".to_owned(),
        ));
    }

    // Initialization creates only the person and their password, signing no API token: a
    // browser needs nothing beyond the session cookie, and a script wanting a token signs one
    // through /admin/operators/{id}/token — a path that already exists.
    sqlx::query(
        "INSERT INTO admin_operators (
            id, display_name, role, tenant_scope, password_hash,
            token_hash, token_prefix, token_created_at,
            token_last_used_at, token_revoked_at
         )
         VALUES ($1, $2, 'system-admin', NULL, $3, NULL, NULL, NULL, NULL, NULL)",
    )
    .bind(&operator_id)
    .bind(&display_name)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let session = issue_admin_session(pool, &operator_id).await?;
    Ok(AdminInitResult {
        admin: AuthenticatedAdmin {
            operator_id,
            role: AdminRole::SystemAdmin,
            tenant_scope: None,
            token_prefix: None,
            self_user: None,
        },
        session,
    })
}

pub async fn login_admin(pool: &PgPool, request: AdminLoginRequest) -> Result<AdminLoginResult> {
    let entered_id = required_text(&request.operator_id, "admin operator id")?;
    let operator_id = resolve_password_login_id(pool, &entered_id).await?;
    let admin = authenticate_admin_password(pool, &operator_id, &request.password)
        .await?
        .ok_or_else(|| StoreError::Unauthorized("invalid admin credentials".to_owned()))?;
    let session = issue_admin_session(pool, &admin.operator_id).await?;
    Ok(AdminLoginResult { admin, session })
}

/// Keep the durable identity unambiguous (`tenant/user`) while making a one-tenant installation
/// pleasant to sign into. An exact operator id always wins, so a short user alias can never
/// shadow an administrator with the same id.
async fn resolve_password_login_id(pool: &PgPool, entered_id: &str) -> Result<String> {
    let exact_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM admin_operators WHERE id = $1)")
            .bind(entered_id)
            .fetch_one(pool)
            .await?;
    if exact_exists {
        return Ok(entered_id.to_owned());
    }

    let alias: Option<String> = sqlx::query_scalar(
        "SELECT o.id
         FROM admin_operators o
         WHERE o.role = 'user'
           AND o.user_id = $1
           AND o.user_tenant_id = (SELECT min(id) FROM tenants)
           AND (SELECT count(*) FROM tenants) = 1
         LIMIT 1",
    )
    .bind(entered_id)
    .fetch_optional(pool)
    .await?;
    Ok(alias.unwrap_or_else(|| entered_id.to_owned()))
}

pub async fn create_admin_operator(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateAdminOperatorRequest,
) -> Result<AdminOperator> {
    if request.role == AdminRole::User {
        return Err(StoreError::InvalidData(
            "user logins must be managed from their user record".to_owned(),
        ));
    }
    validate_operator_request(&request)?;
    let id = request.id.trim();
    let display_name = request.display_name.trim();
    let tenant_scope = request.tenant_scope.as_deref().map(str::trim);
    if id == PUBLIC_OPERATOR_ID && !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can manage the public operator".to_owned(),
        ));
    }
    ensure_admin_operator_write_allowed(pool, actor, id, request.role, tenant_scope).await?;
    if id == PUBLIC_OPERATOR_ID && request.role != AdminRole::Readonly {
        return Err(StoreError::InvalidData(
            "the public operator must have the readonly role".to_owned(),
        ));
    }
    let existing_passwordless: Option<bool> =
        sqlx::query_scalar("SELECT password_hash IS NULL FROM admin_operators WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let will_be_passwordless = request.password.is_none() && existing_passwordless.unwrap_or(true);
    if will_be_passwordless && !(id == PUBLIC_OPERATOR_ID && request.role == AdminRole::Readonly) {
        return Err(StoreError::InvalidData(
            "passwordless login is reserved for the public readonly operator".to_owned(),
        ));
    }
    let password_hash = request
        .password
        .as_deref()
        .map(hash_admin_password)
        .transpose()?;
    // An upsert without a password keeps the existing one: changing a display name should not
    // log anybody out.
    let row = sqlx::query(
        "INSERT INTO admin_operators (id, display_name, role, tenant_scope, password_hash)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET
            display_name = EXCLUDED.display_name,
            role = EXCLUDED.role,
            tenant_scope = EXCLUDED.tenant_scope,
            password_hash = COALESCE(EXCLUDED.password_hash, admin_operators.password_hash)
         RETURNING id,
                   display_name,
                   role,
                   tenant_scope,
                   (password_hash IS NULL) AS passwordless,
                   token_prefix,
                   token_created_at::text AS token_created_at,
                   token_last_used_at::text AS token_last_used_at,
                   token_revoked_at::text AS token_revoked_at",
    )
    .bind(id)
    .bind(display_name)
    .bind(request.role.as_str())
    .bind(tenant_scope)
    .bind(password_hash.as_deref())
    .fetch_one(pool)
    .await?;

    admin_operator_from_row(&row)
}

/// An administrator setting a password on someone's behalf: generate a one-time password,
/// returned to the caller in this response alone. Changing the password invalidates that
/// operator's existing sessions — whoever was reset should be signing in again anyway.
pub async fn reset_admin_password(
    pool: &PgPool,
    actor: &AdminContext,
    operator_id: &str,
) -> Result<ResetAdminPasswordResult> {
    let operator_id = required_text(operator_id, "admin operator id")?;
    ensure_admin_operator_access_allowed(pool, actor, &operator_id).await?;

    let password = generate_admin_password()?;
    let password_hash = hash_admin_password(&password)?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE admin_operators
         SET password_hash = $2,
             token_revoked_at = CASE
                 WHEN token_hash IS NOT NULL THEN COALESCE(token_revoked_at, now())
                 ELSE token_revoked_at
             END
         WHERE id = $1",
    )
    .bind(&operator_id)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;
    let sessions_revoked = revoke_operator_sessions(&mut tx, &operator_id, None).await?;
    tx.commit().await?;

    Ok(ResetAdminPasswordResult {
        operator_id,
        password,
        sessions_revoked,
    })
}

/// Enable a user's console login, or reset it when it already exists. User logins live in the
/// same session system as administrators, while the immutable binding below prevents a password
/// reset from ever turning into access to another user.
pub async fn issue_user_login(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<IssuedUserLogin> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "user login")?;

    let password = generate_admin_password()?;
    let password_hash = hash_admin_password(&password)?;
    let desired_operator_id = format!("{tenant_id}/{user_id}");
    let mut tx = pool.begin().await?;
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM users WHERE tenant_id = $1 AND id = $2)",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }

    let existing_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM admin_operators
         WHERE role = 'user' AND user_tenant_id = $1 AND user_id = $2",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_optional(&mut *tx)
    .await?;

    let mut sessions_revoked = 0;
    let operator_id = if existing_id.as_deref() == Some(desired_operator_id.as_str()) {
        sqlx::query(
            "UPDATE admin_operators
             SET password_hash = $2,
                 token_hash = NULL,
                 token_prefix = NULL,
                 token_created_at = NULL,
                 token_last_used_at = NULL,
                 token_revoked_at = NULL
             WHERE id = $1",
        )
        .bind(&desired_operator_id)
        .bind(&password_hash)
        .execute(&mut *tx)
        .await?;
        desired_operator_id
    } else {
        let collision: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM admin_operators WHERE id = $1)")
                .bind(&desired_operator_id)
                .fetch_one(&mut *tx)
                .await?;
        if collision {
            return Err(StoreError::InvalidData(format!(
                "login name {desired_operator_id} is already in use"
            )));
        }
        // This also upgrades an identity created by the short-lived bare-user naming scheme.
        // Its sessions are invalid after a reset anyway, so remove the old identity only after
        // proving the requested bare login name is available.
        if let Some(existing_id) = existing_id {
            sessions_revoked += revoke_operator_sessions(&mut tx, &existing_id, None).await?;
            sqlx::query("DELETE FROM admin_operators WHERE id = $1")
                .bind(existing_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "INSERT INTO admin_operators (
                id, display_name, role, tenant_scope, password_hash,
                user_tenant_id, user_id
             ) VALUES ($1, $2, 'user', $3, $4, $3, $2)",
        )
        .bind(&desired_operator_id)
        .bind(&user_id)
        .bind(&tenant_id)
        .bind(&password_hash)
        .execute(&mut *tx)
        .await?;
        desired_operator_id
    };

    sessions_revoked += revoke_operator_sessions(&mut tx, &operator_id, None).await?;
    tx.commit().await?;
    Ok(IssuedUserLogin {
        operator_id,
        password,
        sessions_revoked,
    })
}

/// Set a chosen password for an existing user login. This is deliberately separate from
/// `issue_user_login`: reset/enable returns a generated one-time password, while this path accepts
/// the value the operator explicitly entered. Both revoke every existing session and API token.
pub async fn set_user_password(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    request: SetUserPasswordRequest,
) -> Result<SetUserPasswordResult> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "user login")?;
    let password_hash = hash_admin_password(&request.new_password)?;

    let mut tx = pool.begin().await?;
    let operator_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM admin_operators
         WHERE role = 'user' AND user_tenant_id = $1 AND user_id = $2
         FOR UPDATE",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let operator_id = operator_id
        .ok_or_else(|| StoreError::NotFound(format!("user login {tenant_id}/{user_id}")))?;

    sqlx::query(
        "UPDATE admin_operators
         SET password_hash = $2,
             token_revoked_at = CASE
                 WHEN token_hash IS NOT NULL THEN COALESCE(token_revoked_at, now())
                 ELSE token_revoked_at
             END
         WHERE id = $1",
    )
    .bind(&operator_id)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;
    let sessions_revoked = revoke_operator_sessions(&mut tx, &operator_id, None).await?;
    tx.commit().await?;

    Ok(SetUserPasswordResult {
        operator_id,
        sessions_revoked,
    })
}

/// A self-service password change: verify the old password, install the new one, and drop this
/// person's sessions elsewhere along the way. `keep_session_token` is the session that made
/// this request — kept, so that changing a password does not log one out on the spot.
pub async fn change_admin_password(
    pool: &PgPool,
    operator_id: &str,
    keep_session_token: Option<&str>,
    request: ChangeAdminPasswordRequest,
) -> Result<u64> {
    let operator_id = required_text(operator_id, "admin operator id")?;
    if authenticate_admin_password(pool, &operator_id, &request.current_password)
        .await?
        .is_none()
    {
        return Err(StoreError::Unauthorized(
            "current password is incorrect".to_owned(),
        ));
    }
    let password_hash = hash_admin_password(&request.new_password)?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE admin_operators
         SET password_hash = $2,
             token_revoked_at = CASE
                 WHEN token_hash IS NOT NULL THEN COALESCE(token_revoked_at, now())
                 ELSE token_revoked_at
             END
         WHERE id = $1",
    )
    .bind(&operator_id)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;
    let revoked = revoke_operator_sessions(
        &mut tx,
        &operator_id,
        keep_session_token.map(admin_session_token_hash).as_deref(),
    )
    .await?;
    tx.commit().await?;

    Ok(revoked)
}

async fn revoke_operator_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operator_id: &str,
    keep_token_hash: Option<&str>,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE admin_sessions
         SET revoked_at = now()
         WHERE operator_id = $1
           AND revoked_at IS NULL
           AND ($2::text IS NULL OR token_hash <> $2)",
    )
    .bind(operator_id)
    .bind(keep_token_hash)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

pub async fn list_admin_operators(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<Vec<AdminOperator>> {
    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT id,
                    display_name,
                    role,
                    tenant_scope,
                    (password_hash IS NULL) AS passwordless,
                    token_prefix,
                    token_created_at::text AS token_created_at,
                    token_last_used_at::text AS token_last_used_at,
                    token_revoked_at::text AS token_revoked_at
             FROM admin_operators
             WHERE role <> 'user'
             ORDER BY id",
        )
        .fetch_all(pool)
        .await?
    } else {
        if actor.role() != AdminRole::TenantAdmin {
            return Err(StoreError::Forbidden(
                "admin operator list requires tenant-admin".to_owned(),
            ));
        }
        let tenant_scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        let tenant_pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(
            "SELECT id,
                    display_name,
                    role,
                    tenant_scope,
                    (password_hash IS NULL) AS passwordless,
                    token_prefix,
                    token_created_at::text AS token_created_at,
                    token_last_used_at::text AS token_last_used_at,
                    token_revoked_at::text AS token_revoked_at
             FROM admin_operators
             WHERE role NOT IN ('system-admin', 'user')
               AND (tenant_scope = $1 OR tenant_scope LIKE $2 ESCAPE '\\')
             ORDER BY id",
        )
        .bind(tenant_scope)
        .bind(tenant_pattern)
        .fetch_all(pool)
        .await?
    };

    rows.iter().map(admin_operator_from_row).collect()
}

pub async fn issue_admin_token(
    pool: &PgPool,
    actor: &AdminContext,
    operator_id: &str,
) -> Result<IssuedAdminToken> {
    if operator_id.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "operator_id must not be empty when issuing an admin token".to_owned(),
        ));
    }
    ensure_admin_operator_access_allowed(pool, actor, operator_id).await?;
    if load_admin_operator_identity(pool, operator_id)
        .await?
        .is_some_and(|operator| operator.role == AdminRole::User)
    {
        return Err(StoreError::Forbidden(
            "user logins cannot hold admin API tokens".to_owned(),
        ));
    }

    let token = generate_admin_token()?;
    let token_hash = admin_token_hash(&token);
    let token_prefix = admin_token_display_prefix(&token);
    let row = sqlx::query(
        "UPDATE admin_operators
         SET token_hash = $2,
             token_prefix = $3,
             token_created_at = now(),
             token_last_used_at = NULL,
             token_revoked_at = NULL
         WHERE id = $1
         RETURNING id, token_prefix",
    )
    .bind(operator_id)
    .bind(token_hash)
    .bind(&token_prefix)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("admin operator {operator_id}")))?;

    Ok(IssuedAdminToken {
        operator_id: row.try_get("id")?,
        token,
        token_prefix: row.try_get("token_prefix")?,
    })
}

pub async fn authenticate_admin_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<AuthenticatedAdmin>> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }

    let token_hash = admin_token_hash(token);
    let row = sqlx::query(
        "SELECT id, role, tenant_scope, token_prefix, user_tenant_id, user_id,
                (token_last_used_at IS NULL
                 OR token_last_used_at < now() - ($2::int * interval '1 second')) AS touch_last_used
         FROM admin_operators
         WHERE token_hash = $1
           AND token_revoked_at IS NULL",
    )
    .bind(&token_hash)
    .bind(ADMIN_LAST_USED_TOUCH_SECONDS)
    .fetch_optional(pool)
    .await?;

    let touch_last_used = row
        .as_ref()
        .map(|row| row.try_get::<bool, _>("touch_last_used"))
        .transpose()?
        .unwrap_or(false);
    if touch_last_used {
        sqlx::query(
            "UPDATE admin_operators
             SET token_last_used_at = now()
             WHERE token_hash = $1
               AND token_revoked_at IS NULL
               AND (token_last_used_at IS NULL
                    OR token_last_used_at < now() - ($2::int * interval '1 second'))",
        )
        .bind(&token_hash)
        .bind(ADMIN_LAST_USED_TOUCH_SECONDS)
        .execute(pool)
        .await?;
    }

    row.map(|row| {
        Ok(AuthenticatedAdmin {
            operator_id: row.try_get("id")?,
            role: parse_admin_role(row.try_get("role")?)?,
            tenant_scope: row.try_get("tenant_scope")?,
            token_prefix: row.try_get("token_prefix")?,
            self_user: authenticated_user_from_row(&row)?,
        })
    })
    .transpose()
}

pub async fn authenticate_admin_session(
    pool: &PgPool,
    token: &str,
) -> Result<Option<AuthenticatedAdmin>> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }

    let token_hash = admin_session_token_hash(token);
    let row = sqlx::query(
        "SELECT o.id, o.role, o.tenant_scope, o.user_tenant_id, o.user_id,
                (s.last_used_at IS NULL
                 OR s.last_used_at < now() - ($2::int * interval '1 second')) AS touch_last_used
         FROM admin_sessions s
         JOIN admin_operators o ON s.operator_id = o.id
         WHERE s.operator_id = o.id
           AND s.token_hash = $1
           AND s.revoked_at IS NULL
           AND s.expires_at > now()",
    )
    .bind(&token_hash)
    .bind(ADMIN_LAST_USED_TOUCH_SECONDS)
    .fetch_optional(pool)
    .await?;

    let touch_last_used = row
        .as_ref()
        .map(|row| row.try_get::<bool, _>("touch_last_used"))
        .transpose()?
        .unwrap_or(false);
    if touch_last_used {
        sqlx::query(
            "UPDATE admin_sessions
             SET last_used_at = now()
             WHERE token_hash = $1
               AND revoked_at IS NULL
               AND expires_at > now()
               AND (last_used_at IS NULL
                    OR last_used_at < now() - ($2::int * interval '1 second'))",
        )
        .bind(&token_hash)
        .bind(ADMIN_LAST_USED_TOUCH_SECONDS)
        .execute(pool)
        .await?;
    }

    row.map(|row| {
        Ok(AuthenticatedAdmin {
            operator_id: row.try_get("id")?,
            role: parse_admin_role(row.try_get("role")?)?,
            tenant_scope: row.try_get("tenant_scope")?,
            token_prefix: None,
            self_user: authenticated_user_from_row(&row)?,
        })
    })
    .transpose()
}

pub async fn revoke_admin_session(pool: &PgPool, token: &str) -> Result<bool> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(false);
    }
    let token_hash = admin_session_token_hash(token);
    let result = sqlx::query(
        "UPDATE admin_sessions
         SET revoked_at = COALESCE(revoked_at, now())
         WHERE token_hash = $1
           AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn revoke_admin_token(
    pool: &PgPool,
    actor: &AdminContext,
    operator_id: &str,
) -> Result<bool> {
    if operator_id.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "operator_id must not be empty when revoking an admin token".to_owned(),
        ));
    }
    ensure_admin_operator_access_allowed(pool, actor, operator_id).await?;
    let result = sqlx::query(
        "UPDATE admin_operators
         SET token_revoked_at = COALESCE(token_revoked_at, now())
         WHERE id = $1
           AND token_hash IS NOT NULL",
    )
    .bind(operator_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

async fn authenticate_admin_password(
    pool: &PgPool,
    operator_id: &str,
    password: &str,
) -> Result<Option<AuthenticatedAdmin>> {
    let Some(row) = sqlx::query(
        "SELECT id, role, tenant_scope, password_hash, user_tenant_id, user_id
         FROM admin_operators
         WHERE id = $1",
    )
    .bind(operator_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let role = parse_admin_role(row.try_get("role")?)?;
    let password_hash: Option<String> = row.try_get("password_hash")?;
    match password_hash {
        // Apply the invariant while authenticating as well as while writing. That immediately
        // makes legacy unsafe passwordless rows unusable without waiting for a migration.
        None => {
            let id: String = row.try_get("id")?;
            if !password.is_empty() || id != PUBLIC_OPERATOR_ID || role != AdminRole::Readonly {
                return Ok(None);
            }
        }
        Some(password_hash) => {
            if !verify_admin_password(password, &password_hash)? {
                return Ok(None);
            }
        }
    }

    Ok(Some(AuthenticatedAdmin {
        operator_id: row.try_get("id")?,
        role,
        tenant_scope: row.try_get("tenant_scope")?,
        token_prefix: None,
        self_user: authenticated_user_from_row(&row)?,
    }))
}

async fn issue_admin_session(pool: &PgPool, operator_id: &str) -> Result<IssuedAdminSession> {
    let token = generate_admin_session_token()?;
    let token_hash = admin_session_token_hash(&token);
    let expires_at = sqlx::query(
        "INSERT INTO admin_sessions (token_hash, operator_id, expires_at)
         VALUES ($1, $2, now() + ($3::int * interval '1 second'))
         RETURNING expires_at::text AS expires_at",
    )
    .bind(token_hash)
    .bind(operator_id)
    .bind(ADMIN_SESSION_TTL_SECONDS)
    .fetch_one(pool)
    .await?
    .try_get("expires_at")?;
    Ok(IssuedAdminSession { token, expires_at })
}

fn hash_admin_password(password: &str) -> Result<String> {
    validate_admin_password(password)?;
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes)?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| StoreError::InvalidData(format!("invalid password salt: {error}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| StoreError::InvalidData(format!("password hash failed: {error}")))
}

fn verify_admin_password(password: &str, encoded_hash: &str) -> Result<bool> {
    let hash = PasswordHash::new(encoded_hash).map_err(|error| {
        StoreError::InvalidData(format!("stored password hash is invalid: {error}"))
    })?;
    match Argon2::default().verify_password(password.as_bytes(), &hash) {
        Ok(()) => Ok(true),
        Err(PasswordHashError::Password) => Ok(false),
        Err(error) => Err(StoreError::InvalidData(format!(
            "password verification failed: {error}"
        ))),
    }
}

fn validate_admin_password(password: &str) -> Result<()> {
    if password.len() < MIN_ADMIN_PASSWORD_LEN {
        return Err(StoreError::InvalidData(format!(
            "admin password must be at least {MIN_ADMIN_PASSWORD_LEN} bytes"
        )));
    }
    Ok(())
}

/// The two bindings a tenant-scoped query needs, or `None` for an actor who reads everything.
///
/// Returned as a pair because the two placeholders must be supplied together or not at all:
/// computed separately, editing one and forgetting the other has the scoping fail silently while
/// the whole network's rows ship anyway. The `LIKE` pattern is `scope.%` with the escaping rules
/// that live in this module.
pub(crate) fn tenant_filter(actor: &AdminContext) -> Option<(String, String)> {
    if actor.is_system_admin() || actor.is_global_scope() {
        return None;
    }
    let scope = actor.tenant_scope()?.to_owned();
    let pattern = actor.tenant_scope_like_pattern()?;
    Some((scope, pattern))
}

async fn ensure_admin_operator_write_allowed(
    pool: &PgPool,
    actor: &AdminContext,
    operator_id: &str,
    requested_role: AdminRole,
    requested_tenant_scope: Option<&str>,
) -> Result<()> {
    let non_system_tenant_scope = if requested_role != AdminRole::SystemAdmin {
        let scope = requested_tenant_scope.ok_or_else(|| {
            StoreError::InvalidData("non-system admin roles require tenant_scope".to_owned())
        })?;
        ensure_tenant_exists(pool, scope).await?;
        Some(scope)
    } else {
        None
    };

    if actor.is_system_admin() {
        return Ok(());
    }
    if actor.role() != AdminRole::TenantAdmin {
        return Err(StoreError::Forbidden(
            "admin operator management requires tenant-admin".to_owned(),
        ));
    }
    if requested_role == AdminRole::SystemAdmin {
        return Err(StoreError::Forbidden(
            "tenant-admin cannot create or update system-admin operators".to_owned(),
        ));
    }
    let requested_tenant_scope = non_system_tenant_scope.ok_or_else(|| {
        StoreError::InvalidData("non-system admin roles require tenant_scope".to_owned())
    })?;
    actor.require_tenant_access(requested_tenant_scope, "admin operator tenant_scope")?;

    if let Some(existing) = load_admin_operator_identity(pool, operator_id).await? {
        ensure_admin_operator_identity_access(actor, &existing)?;
    }
    Ok(())
}

async fn ensure_tenant_exists(pool: &PgPool, tenant_id: &str) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(pool)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("tenant {tenant_id}")))
}

async fn ensure_admin_operator_access_allowed(
    pool: &PgPool,
    actor: &AdminContext,
    operator_id: &str,
) -> Result<()> {
    if actor.is_system_admin() {
        let exists = sqlx::query("SELECT 1 FROM admin_operators WHERE id = $1")
            .bind(operator_id)
            .fetch_optional(pool)
            .await?
            .is_some();
        return exists
            .then_some(())
            .ok_or_else(|| StoreError::NotFound(format!("admin operator {operator_id}")));
    }
    if actor.role() != AdminRole::TenantAdmin {
        return Err(StoreError::Forbidden(
            "admin operator token management requires tenant-admin".to_owned(),
        ));
    }
    let existing = load_admin_operator_identity(pool, operator_id)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("admin operator {operator_id}")))?;
    ensure_admin_operator_identity_access(actor, &existing)
}

fn ensure_admin_operator_identity_access(
    actor: &AdminContext,
    operator: &AdminOperatorIdentity,
) -> Result<()> {
    if operator.role == AdminRole::SystemAdmin {
        return Err(StoreError::Forbidden(
            "tenant-admin cannot manage system-admin operators".to_owned(),
        ));
    }
    let Some(tenant_scope) = operator.tenant_scope.as_deref() else {
        return Err(StoreError::Forbidden(
            "operator has no tenant_scope".to_owned(),
        ));
    };
    actor.require_tenant_access(tenant_scope, "admin operator")
}

#[derive(Debug)]
struct AdminOperatorIdentity {
    role: AdminRole,
    tenant_scope: Option<String>,
}

async fn load_admin_operator_identity(
    pool: &PgPool,
    operator_id: &str,
) -> Result<Option<AdminOperatorIdentity>> {
    sqlx::query(
        "SELECT role, tenant_scope
         FROM admin_operators
         WHERE id = $1",
    )
    .bind(operator_id)
    .fetch_optional(pool)
    .await?
    .map(|row| {
        Ok(AdminOperatorIdentity {
            role: parse_admin_role(row.try_get("role")?)?,
            tenant_scope: row.try_get("tenant_scope")?,
        })
    })
    .transpose()
}

fn validate_operator_request(request: &CreateAdminOperatorRequest) -> Result<()> {
    if request.role == AdminRole::User {
        return Err(StoreError::InvalidData(
            "user logins must be managed from their user record".to_owned(),
        ));
    }
    if request.id.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "admin operator id must not be empty".to_owned(),
        ));
    }
    if request.display_name.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "admin operator display_name must not be empty".to_owned(),
        ));
    }
    match (request.role, request.tenant_scope.as_deref()) {
        (AdminRole::SystemAdmin, None) => Ok(()),
        (AdminRole::SystemAdmin, Some(_)) => Err(StoreError::InvalidData(
            "system-admin must not have tenant_scope".to_owned(),
        )),
        (_, Some(scope)) if !scope.trim().is_empty() => Ok(()),
        _ => Err(StoreError::InvalidData(
            "non-system admin roles require tenant_scope".to_owned(),
        )),
    }
}

fn normalize_optional_text(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn admin_operator_from_row(row: &sqlx::postgres::PgRow) -> Result<AdminOperator> {
    Ok(AdminOperator {
        id: row.try_get("id")?,
        display_name: row.try_get("display_name")?,
        role: parse_admin_role(row.try_get("role")?)?,
        tenant_scope: row.try_get("tenant_scope")?,
        passwordless: row.try_get("passwordless")?,
        token_prefix: row.try_get("token_prefix")?,
        token_created_at: row.try_get("token_created_at")?,
        token_last_used_at: row.try_get("token_last_used_at")?,
        token_revoked_at: row.try_get("token_revoked_at")?,
    })
}

fn parse_admin_role(value: String) -> Result<AdminRole> {
    match value.as_str() {
        "user" => Ok(AdminRole::User),
        "readonly" => Ok(AdminRole::Readonly),
        "editor" => Ok(AdminRole::Editor),
        "publisher" => Ok(AdminRole::Publisher),
        "tenant-admin" => Ok(AdminRole::TenantAdmin),
        "system-admin" => Ok(AdminRole::SystemAdmin),
        _ => Err(StoreError::InvalidData(format!(
            "unknown admin role {value}"
        ))),
    }
}

fn authenticated_user_from_row(row: &sqlx::postgres::PgRow) -> Result<Option<AuthenticatedUser>> {
    let tenant_id: Option<String> = row.try_get("user_tenant_id")?;
    let user_id: Option<String> = row.try_get("user_id")?;
    match (tenant_id, user_id) {
        (Some(tenant_id), Some(user_id)) => Ok(Some(AuthenticatedUser { tenant_id, user_id })),
        (None, None) => Ok(None),
        _ => Err(StoreError::InvalidData(
            "operator has a partial user binding".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{tenant_in_scope, tenant_scope_like_pattern};

    #[test]
    fn tenant_scope_matches_only_path_segments() {
        assert!(tenant_in_scope("platform.acme", "platform.acme"));
        assert!(tenant_in_scope("platform.acme.child", "platform.acme"));
        assert!(!tenant_in_scope("platform.acme2", "platform.acme"));
        assert!(!tenant_in_scope("platform.acme_child", "platform.acme"));
    }

    #[test]
    fn tenant_scope_like_pattern_escapes_sql_wildcards() {
        assert_eq!(
            tenant_scope_like_pattern("platform.ac_me%beta"),
            r"platform.ac\_me\%beta.%"
        );
    }
}
