pub(crate) mod defaults;
pub(crate) mod keys;
pub(crate) mod model_selection;
pub(crate) mod runtime;
pub(crate) mod summary;
pub(crate) mod workspace;

// Flat re-exports so existing `use crate::config::X` and `use crate::config::*`
// call sites continue to work without modification. Rust does not track wildcard
// consumers and warns spuriously — items ARE used in service/ and telegram/.
#[allow(unused_imports)]
pub(crate) use defaults::{
    load_persisted_telegram_defaults, normalize_nonempty_string, parse_env_bool,
    parse_optional_u64, parse_optional_usize, CliParams, TelegramModeDefaults,
    OPENCODE_API_KEY_KEY, OPENROUTER_API_KEY_KEY, TELEGRAM_ALLOWED_DM_USERNAME_KEY,
    TELEGRAM_BOT_TOKEN_KEY, TELEGRAM_BOUND_DM_USER_ID_KEY, TELEGRAM_SERVICE_UNIT_NAME,
    TOPAGENT_MAX_RETRIES_KEY, TOPAGENT_MAX_STEPS_KEY, TOPAGENT_MODEL_KEY,
    TOPAGENT_PROVIDER_KEY, TOPAGENT_SERVICE_MANAGED_KEY, TOPAGENT_TIMEOUT_SECS_KEY,
    TOPAGENT_TOOL_AUTHORING_KEY, TOPAGENT_WORKSPACE_KEY,
};
#[allow(unused_imports)]
pub(crate) use keys::{
    require_opencode_api_key, require_openrouter_api_key, require_telegram_token,
    require_telegram_token_with_default, resolve_opencode_api_key, resolve_openrouter_api_key,
};
#[allow(unused_imports)]
pub(crate) use model_selection::{
    build_route_from_resolved, canonicalize_allowed_username, current_configured_model,
    provider_or_default, resolve_model_choice, resolve_runtime_model_selection,
    ModelResolutionSource, ResolvedModel, RuntimeModelSelection, SelectedProvider,
};
#[allow(unused_imports)]
pub(crate) use runtime::{
    build_runtime_options, build_runtime_options_with_defaults, resolve_generated_tool_authoring,
    resolve_one_shot_config, resolve_telegram_mode_config, OneShotConfig, TelegramModeConfig,
};
pub(crate) use summary::{resolve_contract_summary, ResolvedContractSummary};
#[allow(unused_imports)]
pub(crate) use workspace::{resolve_workspace_path, resolve_workspace_path_with_current_dir};
