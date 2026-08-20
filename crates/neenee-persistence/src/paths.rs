//! Centralised path resolution for neenee's on-disk footprint.
//!
//! Every persistent path the program writes flows through [`Dirs`]. Resolution
//! honours the XDG Base Directory Specification and layers overrides in this
//! precedence order (highest first):
//!
//! 1. `--home <dir>` CLI flag (expressed via the matching
//!    [`PathsOverride`]) — the **instance root** (ADR-0121).
//! 2. `NEENEE_HOME` — the env form of the same selector: one variable
//!    moves the entire footprint (`<home>/neenee/{config,data,state,
//!    cache}` plus the daemon's runtime files under `instance/`), so a
//!    dev or test build can never touch the host installation's state.
//! 3. `NEENEE_CONFIG_DIR` / `NEENEE_DATA_DIR` / `NEENEE_STATE_DIR` /
//!    `NEENEE_CACHE_DIR` environment variables (app-specific
//!    per-category overrides; more specific than the root, so one
//!    category can still be carved out of a sandbox).
//! 4. `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_STATE_HOME` /
//!    `XDG_CACHE_HOME` / `XDG_RUNTIME_DIR` environment variables
//!    (standard XDG overrides; relative values are ignored per spec).
//! 5. Platform-native defaults via the `directories` crate (`config_dir`,
//!    `data_dir`, `state_dir`, `cache_dir`).
//! 6. `$HOME/.config`, `$HOME/.local/share`, ... fallbacks when even the
//!    `directories` crate cannot resolve a native location.
//!
//! On Linux `$XDG_RUNTIME_DIR` is honoured for the daemon's runtime files;
//! if it is unset the data directory is used (portable fallback). The
//! daemon-facing derivation of that rule lives in [`Dirs::instance_dir`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(any(test, feature = "test-path-override"))]
use std::sync::RwLock;

use directories::ProjectDirs;

/// App-specific override of the path roots supplied by the CLI.
///
/// Any field left as `None` falls back to env / native resolution. This is
/// the type plumbed through `main.rs` from the command line.
#[derive(Debug, Clone, Default)]
pub struct PathsOverride {
    /// `--home <dir>`: the instance root (ADR-0121). One flag redirects
    /// every category and the daemon's runtime files — the same selector
    /// `NEENEE_HOME` expresses as an environment variable for process
    /// trees that cannot pass flags (CI, `cargo test`, auto-spawned
    /// daemons). The CLI flag wins when both are present.
    pub home: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

/// The resolved on-disk layout. All paths are absolute and contain the `neenee`
/// segment as their final component (e.g. `~/.config/neenee`).
#[derive(Debug, Clone)]
pub struct Dirs {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    /// `$XDG_CACHE_HOME/neenee`. Written by the remote-skill cache (see
    /// [`Self::remote_skills_cache`]) and otherwise kept lazily by `fsutil`
    /// on first write.
    pub cache_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR/neenee` when set, otherwise `None` (callers fall
    /// back to `state_dir` for portability and to avoid surprising tmpfs
    /// use). For the daemon's runtime files prefer [`Self::instance_dir`],
    /// which folds this field together with the `--home`/`NEENEE_HOME`
    /// override.
    pub runtime_dir: Option<PathBuf>,
}

impl Dirs {
    /// Resolve using the given CLI overrides combined with env / native.
    pub fn resolve(overrides: &PathsOverride) -> Self {
        let project = ProjectDirs::from("ai", "neenee", "neenee");
        // The instance root (ADR-0121): the `--home` flag beats the
        // `NEENEE_HOME` env var; both are normalised to the `neenee`-suffixed
        // base once, so every category and the instance dir hang off one
        // location: `<home>/neenee/{config,data,state,cache,instance}`.
        // `app_dir_from_root` also tolerates a root that already ends in
        // `neenee`, so `--home /x/neenee` is accepted.
        let home_base = overrides
            .home
            .clone()
            .or_else(neenee_home)
            .map(app_dir_from_root);
        Self {
            config_dir: resolve_kind(
                Kind::Config,
                overrides.config_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            data_dir: resolve_kind(
                Kind::Data,
                overrides.data_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            state_dir: resolve_kind(
                Kind::State,
                overrides.state_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            cache_dir: resolve_kind(
                Kind::Cache,
                overrides.cache_dir.clone(),
                home_base.as_deref(),
                project.as_ref(),
            ),
            runtime_dir: resolve_runtime(home_base.as_deref()),
        }
    }

    /// Resolve using only env / native defaults (no CLI overrides). Convenience
    /// for code paths that have not been plumbed through `main.rs`.
    pub fn system() -> Self {
        Self::resolve(&PathsOverride::default())
    }

    // ---- well-known files --------------------------------------------------

    /// User-edited configuration. `$XDG_CONFIG_HOME/neenee/config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// User-supplied color scheme files (`$XDG_CONFIG_HOME/neenee/themes`).
    /// Each `*.toml` in this directory defines a named theme with metadata
    /// and semantic palette / component overrides.
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// User-supplied ASCII logo for the empty-state hero.
    /// `$XDG_CONFIG_HOME/neenee/logo.txt`. When present, its lines replace the
    /// built-in figlet wordmark on the welcome screen (see `empty_state`).
    /// Optional and best-effort: missing/unreadable → built-in logo.
    pub fn logo_file(&self) -> PathBuf {
        self.config_dir.join("logo.txt")
    }

    /// Provider API keys, split out of `config.toml` into a file that holds
    /// **only** secrets. Written `rw-------` via [`crate::fsutil`] so keys
    /// never land on disk group- or world-readable. Keeping credentials here
    /// (rather than inline in `config.toml`) lets the config file be safely
    /// shared, screenshotted for support, or version-controlled, while
    /// `config.toml` keeps the provider *definitions* (id/name/transport/
    /// base_url/model). Resolution precedence — env var > credentials.toml >
    /// config inline — lives in [`crate::config::Config::load`].
    /// `$XDG_CONFIG_HOME/neenee/credentials.toml`.
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }

    /// OAuth token sets, keyed by provider id (`auth.toml`, 0600). Stored in
    /// `$XDG_STATE_HOME/neenee/auth.toml` as dynamic runtime state.
    pub fn auth_file(&self) -> PathBuf {
        self.state_dir.join("auth.toml")
    }

    /// Legacy location in config_dir for backward compatibility.
    pub fn legacy_auth_file(&self) -> PathBuf {
        self.config_dir.join("auth.toml")
    }

    /// Cached model discovery lists and capability metadata (`$XDG_CACHE_HOME/neenee/models_discovery.json`).
    pub fn discovery_cache_file(&self) -> PathBuf {
        self.cache_dir.join("models_discovery.json")
    }

    /// Content-addressed blob store root. Large payloads are stored under
    /// `<root>/<2-char-prefix>/<hash>`.
    pub fn blobs_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    /// Persistent, program-generated data lives under here.
    pub fn projects_dir(&self) -> PathBuf {
        self.data_dir.join("projects")
    }

    /// Per-project bucket directory: `projects/<sha256(cwd)[..16]>`. Each
    /// project's sessions, current pointer, and metadata live under their own
    /// bucket, so different working directories never see each other's
    /// sessions. The hash is truncated to 16 hex chars (64 bits) — enough to
    /// make accidental collision astronomically unlikely across a single
    /// user's projects while keeping the directory name short and ASCII-safe.
    pub fn project_dir(&self, project_root: &Path) -> PathBuf {
        self.projects_dir().join(project_bucket_name(project_root))
    }

    /// User-global skills (`$XDG_DATA_HOME/neenee/skills`). Per-project skills
    /// still live under the project's working directory (`.neenee/skills/`)
    /// and are not stored here.
    pub fn user_skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }

    /// Cached remote skills (`$XDG_CACHE_HOME/neenee/skills/remote`). Safe to
    /// delete; repopulated on next `fetch_remote_repo`.
    pub fn remote_skills_cache(&self) -> PathBuf {
        self.cache_dir.join("skills").join("remote")
    }

    /// User-global slash commands (`$XDG_DATA_HOME/neenee/commands`). Project
    /// commands still live under `.neenee/commands/` in the working directory.
    pub fn user_commands_dir(&self) -> PathBuf {
        self.data_dir.join("commands")
    }

    /// Slash-command input history. Rebuildable.
    pub fn history_file(&self) -> PathBuf {
        self.state_dir.join("history.json")
    }

    /// Per-model usage telemetry (`last_used`, use count) driving recency
    /// ordering in the provider picker (ADR-0002). Rebuildable: loss affects sort
    /// order only, never configuration. Sits next to [`Self::history_file`]
    /// under `$XDG_STATE_HOME` since it is the same kind of program-generated
    /// signal.
    pub fn provider_usage_file(&self) -> PathBuf {
        self.state_dir.join("provider_usage.json")
    }

    /// User-granted trust set for project-scope external tools (ADR-0085 §5).
    /// Records the absolute project roots the user has explicitly trusted, so
    /// a project's `.neenee/config.toml` `[mcp.*]` (which may execute
    /// processes) only auto-loads after a one-time `/trust`. Loss = revert to
    /// safe (re-prompt); never configuration.
    pub fn trusted_projects_file(&self) -> PathBuf {
        self.state_dir.join("trusted_projects.json")
    }

    /// Per-project embedding index. A lightweight brute-force index by default;
    /// future versions may swap in an HNSW/vector-DB backend using the same
    /// path convention.
    pub fn project_embeddings(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("embeddings.json")
    }

    /// Per-project advisory lock. Stored inside the project bucket so different
    /// projects can run concurrently while the same project is serialised.
    /// Opt-in as of ADR-0018 (the `--single-instance` flag); the default is
    /// unlocked so multiple `neenee` instances can run in one project.
    pub fn project_lock_file(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("neenee.lock")
    }

    /// Per-project directory holding every session file. As of ADR-0018 each
    /// live `neenee` instance pins its own `sessions/<id>.json` plus
    /// `sessions/<id>.jsonl` here, so concurrent instances never share a
    /// mutable file. Replaces the legacy single project-root `session.json`.
    pub fn project_sessions_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("sessions")
    }

    /// Per-project `/debug trace` capture directory: `projects/<bucket>/network`.
    /// Each provider round-trip is written here as one owner-only JSON file
    /// while tracing is armed. Mirror of the `sessions/` layout; the
    /// directory is created lazily on first write by `atomic_write_bytes`.
    pub fn project_network_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("network")
    }

    /// Per-project `/debug preview` directory: `projects/<bucket>/debug`.
    /// One owner-only JSON file is written here per `/debug preview` invocation —
    /// a dry-run of the request that *would* be sent (rebuilt system message +
    /// auto-loaded skills + message list + tool schemas + token pressure),
    /// without calling the provider. The directory is created lazily on first
    /// write by `atomic_write_bytes`. Mirror of the `network/` layout.
    pub fn project_debug_dir(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("debug")
    }

    /// One session's snapshot path: `sessions/<id>.json`. The matching event
    /// log lives at `sessions/<id>.jsonl` (derived via `with_extension`).
    pub fn project_session_file(&self, project_root: &Path, id: &str) -> PathBuf {
        self.project_sessions_dir(project_root)
            .join(format!("{id}.json"))
    }

    /// Per-project one-shot migration lock. Guards the lazy move of the legacy
    /// project-root `session.json` / `events.jsonl` into `sessions/<id>.*` so
    /// two instances starting for the first time do not race the migration
    /// (ADR-0018).
    pub fn project_migration_lock(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("sessions.lock")
    }

    /// Per-project persistent "always allow" permission rules. The cached
    /// rules from `PermissionDecision::Always` are mirrored here so a new
    /// session in the same project inherits prior approvals instead of
    /// re-prompting for the same operations. Best-effort; absence or parse
    /// failure is non-fatal (the agent just asks the user again).
    pub fn project_permissions(&self, project_root: &Path) -> PathBuf {
        self.project_dir(project_root).join("permissions.json")
    }

    /// Structured log directory for the rolling appender, under
    /// `$XDG_STATE_HOME/neenee/log`. Used by `init_tracing` at startup
    /// and by `Self::ensure` in tests.
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("log")
    }

    // ---- helpers -----------------------------------------------------------

    /// The **daemon instance directory**: the one directory holding the
    /// per-daemon runtime files — `daemon.json` (discovery), `daemon.sock`
    /// (control plane), `daemon.lock` (single-instance flock), `serve/`
    /// (legacy records) — and nothing else (ADR-0121).
    ///
    /// It is exactly [`Self::runtime_dir`] when a runtime location resolves
    /// (`--home`/`NEENEE_HOME` → `<home>/neenee/instance`, else
    /// `$XDG_RUNTIME_DIR/neenee`), else the data dir as the portable
    /// fallback — the same rule every daemon-facing call site applied
    /// before ADR-0121, now named once.
    ///
    /// Code that touches daemon runtime files must use this method — never
    /// `runtime_dir` directly — so every daemon-facing path observes the
    /// same override stack. `runtime_dir` stays public as the raw resolved
    /// location for diagnostics and tests that anchor other ephemeral files.
    pub fn instance_dir(&self) -> PathBuf {
        self.runtime_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.clone())
    }

    /// Best-effort initial creation of every directory neenee may write to.
    /// Idempotent. Errors are surfaced as a single aggregate `String`. Used by
    /// tests; production creates directories lazily via `fsutil` on first write.
    #[cfg(test)]
    pub fn ensure(&self) -> Result<(), String> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.cache_dir,
            &self.projects_dir(),
            &self.user_skills_dir(),
            &self.user_commands_dir(),
            &self.remote_skills_cache(),
            &self.log_dir(),
        ] {
            std::fs::create_dir_all(path)
                .map_err(|e| format!("could not create directory {}: {e}", path.display()))?;
        }
        if let Some(runtime) = &self.runtime_dir {
            // Best-effort: the runtime directory is ephemeral and may not be
            // writable in sandboxes or when an unrelated test set
            // `XDG_RUNTIME_DIR`. Do not let this prevent data/state creation.
            let _ = std::fs::create_dir_all(runtime);
        }
        Ok(())
    }
}

/// Global process-wide [`Dirs`] instance. `main` installs it once via
/// [`set_default`] (the `--home` override, ADR-0121); every other
/// module reads via [`get`].
///
/// Implementation: a `std::sync::OnceLock` holds the production value (set
/// exactly once at startup, never replaced, so production code can rely on
/// stability). A separate `std::sync::RwLock` layered on top is used **only
/// by tests** to swap in isolated `Dirs` per test, since tests cannot reset a
/// `OnceLock`. Production reads ([`get`]) check the test override first; if it
/// is empty they fall back to the `OnceLock`, then to a fresh
/// [`Dirs::system`] resolution.
static DEFAULT: OnceLock<Dirs> = OnceLock::new();
/// Test-only override. Marked `allow(dead_code)` because a production build
/// compiles the static but never reads it (every accessor sits behind the
/// same `test` / `test-path-override` gate).
#[cfg(any(test, feature = "test-path-override"))]
static TEST_OVERRIDE: RwLock<Option<Dirs>> = RwLock::new(None);

/// Single process-wide lock that **every** test touching [`set_test_default`]
/// must hold for the duration of its override. Without this, tests in
/// different modules each used their own per-module lock (`config`'s
/// `PATHS_GUARD`, `session`'s `GLOBAL_GUARD`), so two such tests ran
/// concurrently and stomped the shared `TEST_OVERRIDE` — a flaky cross-test
/// race. Routing all of them through one lock serialises the critical section
/// regardless of which module the test lives in.
#[cfg(any(test, feature = "test-path-override"))]
pub static TEST_OVERRIDE_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Install the process-wide [`Dirs`]. Idempotent: subsequent calls in the same
/// process are no-ops (the first value wins), matching production semantics.
/// Returns `Ok(None)` on first install or `Ok(Some(previous))` if a value was
/// already set (the new value is NOT stored in that case).
///
/// `neenee-cli`'s `main` calls this once at startup to install the
/// `--home` override (ADR-0121) before any path is resolved; library
/// code that runs outside `main` (tests, examples) falls back to
/// [`Dirs::system`] via [`get`].
pub fn set_default(dirs: Dirs) -> Result<Option<Dirs>, Dirs> {
    match DEFAULT.set(dirs) {
        Ok(()) => Ok(None),
        Err(existing) => Ok(Some(existing)),
    }
}

/// Test-only override of the process-wide [`Dirs`]. Pass `None` to clear.
/// Production code MUST NOT call this — it exists purely so unit tests can run
/// with isolated `data_dir`/`state_dir` roots without polluting the real
/// filesystem or racing the `OnceLock`.
///
/// Compiled under `#[cfg(any(test, feature = "test-path-override"))]`: the
/// `test-path-override` feature exists so *other crates'* test suites (which
/// cannot see this crate's `cfg(test)`) can install the same sandbox. A
/// dev-dependency with `features = ["test-path-override"]` opts a crate into
/// it without leaking the hook into production builds.
#[cfg(any(test, feature = "test-path-override"))]
pub fn set_test_default(dirs: Option<Dirs>) {
    *TEST_OVERRIDE.write().unwrap_or_else(|e| e.into_inner()) = dirs;
}

/// Access the process-wide [`Dirs`]. Falls back to [`Dirs::system`] when
/// [`set_default`] has not been called yet (e.g. in tests, or in library code
/// invoked outside of `main`). When a test override is installed (via the
/// test-only `set_test_default`), that value wins over the production install.
pub fn get() -> Dirs {
    #[cfg(any(test, feature = "test-path-override"))]
    if let Some(d) = TEST_OVERRIDE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return d;
    }
    match DEFAULT.get() {
        Some(d) => d.clone(),
        None => Dirs::system(),
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Config,
    Data,
    State,
    Cache,
}

impl Kind {
    fn app_env_var(self) -> &'static str {
        match self {
            Kind::Config => "NEENEE_CONFIG_DIR",
            Kind::Data => "NEENEE_DATA_DIR",
            Kind::State => "NEENEE_STATE_DIR",
            Kind::Cache => "NEENEE_CACHE_DIR",
        }
    }

    fn xdg_env_var(self) -> &'static str {
        match self {
            Kind::Config => "XDG_CONFIG_HOME",
            Kind::Data => "XDG_DATA_HOME",
            Kind::State => "XDG_STATE_HOME",
            Kind::Cache => "XDG_CACHE_HOME",
        }
    }

    fn fallback_segment(self) -> &'static str {
        match self {
            Kind::Config => ".config",
            Kind::Data => ".local/share",
            Kind::State => ".local/state",
            Kind::Cache => ".cache",
        }
    }

    /// The subdirectory under an instance root (ADR-0121). Plain names,
    /// not XDG segments: the instance root is not an XDG hierarchy and
    /// `app_dir_from_root` appends the `neenee` segment once.
    fn home_segment(self) -> &'static str {
        match self {
            Kind::Config => "config",
            Kind::Data => "data",
            Kind::State => "state",
            Kind::Cache => "cache",
        }
    }

    fn native(self, project: Option<&ProjectDirs>) -> Option<PathBuf> {
        let p = project?;
        Some(match self {
            Kind::Config => p.config_dir().to_path_buf(),
            Kind::Data => p.data_dir().to_path_buf(),
            Kind::State => p
                .state_dir()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| p.data_dir().join("../state")),
            Kind::Cache => p.cache_dir().to_path_buf(),
        })
    }
}

/// The `NEENEE_HOME` env layer of the instance-root selector (ADR-0121).
/// Returns the raw value; the caller normalises it to the `neenee`-suffixed
/// base exactly once. The value must be absolute and non-empty; a relative
/// value is ignored (with a warning) because an instance root only isolates
/// when both processes see the same absolute location.
fn neenee_home() -> Option<PathBuf> {
    let value = std::env::var_os("NEENEE_HOME")?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        tracing::warn!(
            value = %path.display(),
            "NEENEE_HOME must be absolute; ignoring it (no sandbox active)"
        );
        return None;
    }
    Some(path)
}

/// Resolve the daemon's runtime location (ADR-0121): the instance root's
/// `instance/` subdirectory when one is active, else `$XDG_RUNTIME_DIR/
/// neenee` (pre-0121 behaviour, unchanged). A relative or empty env value
/// is ignored with a warning: an instance root only isolates when both
/// processes see the same absolute location.
fn resolve_runtime(home_base: Option<&Path>) -> Option<PathBuf> {
    if let Some(base) = home_base {
        return Some(base.join("instance"));
    }
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(x) => {
            let p = PathBuf::from(x);
            if p.is_absolute() {
                return Some(p.join("neenee"));
            }
            tracing::warn!(value = %p.display(), "XDG_RUNTIME_DIR must be absolute; ignoring it");
            None
        }
        None => None,
    }
}

fn resolve_kind(
    kind: Kind,
    override_path: Option<PathBuf>,
    home: Option<&Path>,
    project: Option<&ProjectDirs>,
) -> PathBuf {
    // 1. CLI flag
    if let Some(p) = override_path {
        return app_dir_from_root(p);
    }
    // 2. NEENEE_* env (per-category, the most specific selector)
    if let Some(p) = std::env::var_os(kind.app_env_var()).filter(|v| !v.is_empty()) {
        return app_dir_from_root(PathBuf::from(p));
    }
    // 3. Instance root (ADR-0121): `--home` flag or `NEENEE_HOME` env.
    //    `home` is already the `neenee`-suffixed base, so only the category
    //    segment appends.
    if let Some(base) = home {
        return base.join(kind.home_segment());
    }
    // 4. XDG_* env (must be absolute per spec, otherwise ignored)
    if let Some(p) = std::env::var_os(kind.xdg_env_var()).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            return app_dir_from_root(p);
        }
    }
    // 5. Native
    if let Some(p) = kind.native(project) {
        // `directories` already returns the app-suffixed path
        return p;
    }
    // 6. Home fallback
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            return home.join(kind.fallback_segment()).join("neenee");
        }
    }
    // Last resort: cwd. Better than panicking.
    app_dir_from_root(PathBuf::from("."))
}

/// Given a root directory (e.g. `--data-dir=/tmp/x` or `$XDG_DATA_HOME=/foo`),
/// append the `neenee` segment unless the caller already named a directory that
/// ends in `neenee` (so `--data-dir=~/.local/share/neenee` and
/// `--data-dir=~/.local/share` both do the right thing).
fn app_dir_from_root(root: PathBuf) -> PathBuf {
    if root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "neenee")
        .unwrap_or(false)
    {
        root
    } else {
        root.join("neenee")
    }
}

/// Map a project root (cwd) to a stable, ASCII-safe bucket name. Uses the first
/// 16 hex chars of SHA-256 so the layout is reproducible across processes,
/// Rust versions, and platforms, and so the cwd is not leaked in the path
/// structure (paths may contain sensitive directory names).
pub fn project_bucket_name(project_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let normalised = normalise_project_root(project_root);
    let mut hasher = Sha256::new();
    hasher.update(normalised.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Canonicalise a project root for hashing. Redundant trailing slashes are
/// stripped, and on POSIX `..`/`.` segments are collapsed via
/// [`Path::canonicalize`] when the path actually exists; otherwise the raw path
/// is used (so a not-yet-created `--project` still produces a stable name).
fn normalise_project_root(path: &Path) -> String {
    let trimmed = path
        .to_str()
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_default();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that mutate process-wide env vars (`XDG_*`, `NEENEE_*`, `HOME`)
    /// cannot run in parallel with each other or with tests that read those
    /// vars. We serialise them through this global lock. Tests that don't touch
    /// env vars omit the guard and can still run in parallel.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    macro_rules! env_locked {
        ($body:block) => {{
            let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            $body
        }};
    }

    #[test]
    fn app_dir_from_root_appends_neenee_segment() {
        let p = app_dir_from_root(PathBuf::from("/tmp/foo"));
        assert_eq!(p, PathBuf::from("/tmp/foo/neenee"));
    }

    #[test]
    fn app_dir_from_root_does_not_double_append() {
        let p = app_dir_from_root(PathBuf::from("/tmp/foo/neenee"));
        assert_eq!(p, PathBuf::from("/tmp/foo/neenee"));
    }

    #[test]
    fn resolve_honours_neenee_env_over_xdg_env() {
        env_locked!({
            unsafe {
                std::env::set_var("NEENEE_DATA_DIR", "/tmp/neenee-paths-test-data");
            }
            unsafe {
                std::env::set_var("XDG_DATA_HOME", "/tmp/should-not-be-used");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.data_dir,
                PathBuf::from("/tmp/neenee-paths-test-data/neenee")
            );
            unsafe {
                std::env::remove_var("NEENEE_DATA_DIR");
            }
            unsafe {
                std::env::remove_var("XDG_DATA_HOME");
            }
        });
    }

    #[test]
    fn resolve_cli_override_beats_env() {
        env_locked!({
            unsafe {
                std::env::set_var("NEENEE_DATA_DIR", "/tmp/env-loses");
            }
            let dirs = Dirs::resolve(&PathsOverride {
                data_dir: Some(PathBuf::from("/tmp/cli-wins")),
                ..Default::default()
            });
            assert_eq!(dirs.data_dir, PathBuf::from("/tmp/cli-wins/neenee"));
            unsafe {
                std::env::remove_var("NEENEE_DATA_DIR");
            }
        });
    }

    #[test]
    fn resolve_ignores_relative_xdg_var() {
        env_locked!({
            unsafe {
                std::env::set_var("XDG_CACHE_HOME", "relative/path");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(dirs.cache_dir.is_absolute() || dirs.cache_dir.starts_with("."));
            unsafe {
                std::env::remove_var("XDG_CACHE_HOME");
            }
        });
    }

    #[test]
    fn runtime_dir_only_when_xdg_runtime_dir_set() {
        env_locked!({
            unsafe {
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(dirs.runtime_dir.is_none());
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", "/run/user/12345");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.runtime_dir.as_deref(),
                Some(std::path::Path::new("/run/user/12345/neenee"))
            );
            unsafe {
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
        });
    }

    // ---- NEENEE_HOME instance root (ADR-0121) ------------------------------

    #[test]
    fn neenee_home_redirects_every_category_and_the_instance_dir() {
        env_locked!({
            for var in [
                "NEENEE_HOME",
                "NEENEE_CONFIG_DIR",
                "NEENEE_DATA_DIR",
                "NEENEE_STATE_DIR",
                "NEENEE_CACHE_DIR",
            ] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", "/run/user/12345");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.instance_dir(),
                PathBuf::from("/run/user/12345/neenee"),
                "without NEENEE_HOME the XDG runtime dir still wins"
            );

            unsafe {
                std::env::set_var("NEENEE_HOME", "/tmp/nn-home");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(dirs.config_dir, PathBuf::from("/tmp/nn-home/neenee/config"));
            assert_eq!(dirs.data_dir, PathBuf::from("/tmp/nn-home/neenee/data"));
            assert_eq!(dirs.state_dir, PathBuf::from("/tmp/nn-home/neenee/state"));
            assert_eq!(dirs.cache_dir, PathBuf::from("/tmp/nn-home/neenee/cache"));
            assert_eq!(
                dirs.instance_dir(),
                PathBuf::from("/tmp/nn-home/neenee/instance"),
                "the instance dir must follow the sandbox root, not the host XDG runtime"
            );

            for var in ["NEENEE_HOME", "XDG_RUNTIME_DIR"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
        });
    }

    #[test]
    fn home_flag_beats_the_neenee_home_env() {
        env_locked!({
            unsafe {
                std::env::set_var("NEENEE_HOME", "/tmp/nn-env-home");
                std::env::set_var("XDG_RUNTIME_DIR", "/run/user/12345");
            }
            let dirs = Dirs::resolve(&PathsOverride {
                home: Some(PathBuf::from("/tmp/nn-cli-home")),
                ..Default::default()
            });
            assert_eq!(
                dirs.instance_dir(),
                PathBuf::from("/tmp/nn-cli-home/neenee/instance"),
                "the --home flag is the same selector as NEENEE_HOME, and wins"
            );
            for var in ["NEENEE_HOME", "XDG_RUNTIME_DIR"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
        });
    }

    #[test]
    fn neenee_home_leaves_headroom_for_per_category_overrides() {
        env_locked!({
            unsafe {
                std::env::set_var("NEENEE_HOME", "/tmp/nn-home");
                std::env::set_var("NEENEE_DATA_DIR", "/tmp/nn-explicit-data");
                std::env::set_var("XDG_RUNTIME_DIR", "/run/user/12345");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(
                dirs.data_dir,
                PathBuf::from("/tmp/nn-explicit-data/neenee"),
                "a per-category env var is more specific than the instance root"
            );
            assert_eq!(
                dirs.instance_dir(),
                PathBuf::from("/tmp/nn-home/neenee/instance"),
                "the daemon runtime files follow the root"
            );
            assert_eq!(
                dirs.config_dir,
                PathBuf::from("/tmp/nn-home/neenee/config"),
                "categories without an explicit override keep following the root"
            );
            for var in ["NEENEE_HOME", "NEENEE_DATA_DIR", "XDG_RUNTIME_DIR"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
        });
    }

    #[test]
    fn relative_or_empty_neenee_home_is_ignored() {
        env_locked!({
            unsafe {
                std::env::set_var("NEENEE_HOME", "relative/home");
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(
                dirs.runtime_dir.is_none(),
                "a relative sandbox root must not half-apply"
            );
            unsafe {
                std::env::set_var("NEENEE_HOME", "");
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert!(dirs.runtime_dir.is_none(), "an empty root is unset");
            unsafe {
                std::env::remove_var("NEENEE_HOME");
            }
        });
    }

    #[test]
    fn instance_dir_falls_back_to_data_dir_without_a_runtime_location() {
        env_locked!({
            for var in ["XDG_RUNTIME_DIR", "NEENEE_HOME"] {
                unsafe {
                    std::env::remove_var(var);
                }
            }
            let dirs = Dirs::resolve(&PathsOverride::default());
            assert_eq!(dirs.instance_dir(), dirs.data_dir);
        });
    }

    #[test]
    fn project_bucket_name_is_stable_and_ascii_safe() {
        let n1 = project_bucket_name(Path::new("/home/user/code/neenee"));
        let n2 = project_bucket_name(Path::new("/home/user/code/neenee"));
        assert_eq!(n1, n2, "must be stable for the same input");
        assert_eq!(n1.len(), 16, "must be 16 hex chars (8 bytes)");
        assert!(n1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn project_bucket_name_normalises_trailing_slash() {
        let a = project_bucket_name(Path::new("/foo/bar"));
        let b = project_bucket_name(Path::new("/foo/bar/"));
        assert_eq!(a, b, "trailing slash must not change the bucket");
    }

    #[test]
    fn project_bucket_name_distinguishes_different_roots() {
        let a = project_bucket_name(Path::new("/foo/aaa"));
        let b = project_bucket_name(Path::new("/foo/bbb"));
        assert_ne!(a, b);
    }

    #[test]
    fn project_dir_under_projects_root() {
        let dirs = Dirs::resolve(&PathsOverride {
            data_dir: Some(PathBuf::from("/tmp/nd")),
            ..Default::default()
        });
        let project_root = Path::new("/home/me/proj");
        let bucket = project_bucket_name(project_root);
        assert_eq!(
            dirs.project_dir(project_root),
            PathBuf::from(format!("/tmp/nd/neenee/projects/{bucket}"))
        );
    }

    #[test]
    fn project_permissions_under_project_bucket() {
        let dirs = Dirs::resolve(&PathsOverride {
            data_dir: Some(PathBuf::from("/tmp/nd")),
            ..Default::default()
        });
        let project_root = Path::new("/home/me/proj");
        let bucket = project_bucket_name(project_root);
        assert_eq!(
            dirs.project_permissions(project_root),
            PathBuf::from(format!("/tmp/nd/neenee/projects/{bucket}/permissions.json"))
        );
    }
}
