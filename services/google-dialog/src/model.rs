//! Model-specific semantics introduced by Gemini 3.8 Live.
//!
//! Gemini 3.8 adds interleaved/background reasoning, explicit interaction
//! lifecycle status, asynchronous function behavior, function-response
//! scheduling, and incremental `clientContent` updates. The transport and
//! event mappings for lifecycle and client content live in `client.rs`; this
//! module owns the setup and tool-policy differences.

use anyhow::{Result, bail};
use gemini_live::types::{FunctionBehavior, FunctionResponseScheduling, ThinkingLevel, Tool};

use crate::Params;

/// Standard Gemini 3.8 Live: interleaved reasoning without configurable
/// `thinking_level`, with blocking or non-blocking tools and scheduled
/// non-blocking responses.
pub const GEMINI_3_8_LIVE: &str = "gemini-3.8-live";
/// Gemini 3.8 Live Extended Thinking: configurable background reasoning,
/// non-blocking tools only, and interaction lifecycle status updates.
pub const GEMINI_3_8_LIVE_EXTENDED_THINKING: &str = "gemini-3.8-live-extended-thinking";

#[derive(Debug, Clone, Copy)]
struct ModelConfig {
    thinking: ThinkingPolicy,
    functions: FunctionPolicy,
    response_scheduling: SchedulingPolicy,
}

#[derive(Debug, Clone, Copy)]
enum ThinkingPolicy {
    Disabled,
    Optional { allows_minimal: bool },
}

#[derive(Debug, Clone, Copy)]
enum FunctionPolicy {
    BlockingAndNonBlocking,
    NonBlockingOnly,
}

#[derive(Debug, Clone, Copy)]
enum SchedulingPolicy {
    SupportedForNonBlocking,
    Unsupported,
}

fn model_config(model: &str) -> Option<ModelConfig> {
    match model {
        GEMINI_3_8_LIVE => Some(ModelConfig {
            // Fixed interleaved reasoning; `thinking_level` is not accepted.
            thinking: ThinkingPolicy::Disabled,
            // Both legacy blocking and asynchronous tools are supported.
            functions: FunctionPolicy::BlockingAndNonBlocking,
            // `INTERRUPT`, `WHEN_IDLE`, and `SILENT` are supported for
            // non-blocking function responses.
            response_scheduling: SchedulingPolicy::SupportedForNonBlocking,
        }),
        GEMINI_3_8_LIVE_EXTENDED_THINKING => Some(ModelConfig {
            // Background reasoning accepts low, medium, and high levels, but
            // not `minimal`.
            thinking: ThinkingPolicy::Optional {
                allows_minimal: false,
            },
            // Tools must run in the background while the model reasons.
            functions: FunctionPolicy::NonBlockingOnly,
            // Function-response scheduling is not accepted.
            response_scheduling: SchedulingPolicy::Unsupported,
        }),
        _ => None,
    }
}

pub fn validate_thinking_level(params: &Params) -> Result<()> {
    let Some(config) = model_config(&params.model) else {
        return Ok(());
    };

    match (config.thinking, params.thinking_level) {
        (ThinkingPolicy::Disabled, Some(level)) => bail!(
            "Model `{}` does not support thinking_level `{level}`; omit thinking_level",
            params.model
        ),
        (ThinkingPolicy::Optional { allows_minimal }, Some(ThinkingLevel::Minimal))
            if !allows_minimal =>
        {
            bail!(
                "Model `{}` does not support thinking_level `minimal`",
                params.model
            )
        }
        _ => Ok(()),
    }
}

pub fn tools_for_model(model: &str, input_tools: &[Tool]) -> Result<Vec<Tool>> {
    let mut tools = input_tools.to_vec();
    let Some(config) = model_config(model) else {
        return Ok(tools);
    };
    if !matches!(config.functions, FunctionPolicy::NonBlockingOnly) {
        return Ok(tools);
    }

    for tool in &mut tools {
        let Tool::FunctionDeclarations(declarations) = tool else {
            continue;
        };
        for declaration in declarations {
            if declaration.scheduling.is_some() {
                bail!(
                    "Model `{}` does not support function declaration scheduling",
                    model
                );
            }
            match declaration.behavior.clone() {
                Some(FunctionBehavior::Blocking) => bail!(
                    "Model `{}` requires non-blocking function declarations",
                    model
                ),
                None => declaration.behavior = Some(FunctionBehavior::NonBlocking),
                Some(FunctionBehavior::NonBlocking) => {}
            }
        }
    }
    Ok(tools)
}

pub fn validate_response_scheduling(
    params: &Params,
    function_name: &str,
    scheduling: Option<&FunctionResponseScheduling>,
) -> Result<()> {
    let Some(_scheduling) = scheduling else {
        return Ok(());
    };
    let Some(config) = model_config(&params.model) else {
        return Ok(());
    };
    match config.response_scheduling {
        SchedulingPolicy::Unsupported => bail!(
            "Model `{}` does not support function response scheduling",
            params.model
        ),
        SchedulingPolicy::SupportedForNonBlocking
            if function_behavior(&params.tools, function_name)
                .unwrap_or(FunctionBehavior::Blocking)
                == FunctionBehavior::Blocking =>
        {
            bail!(
                "Function response scheduling is not valid for blocking function `{function_name}`"
            )
        }
        SchedulingPolicy::SupportedForNonBlocking => {}
    }
    Ok(())
}

fn function_behavior(tools: &[Tool], function_name: &str) -> Option<FunctionBehavior> {
    tools.iter().find_map(|tool| {
        let Tool::FunctionDeclarations(declarations) = tool else {
            return None;
        };
        declarations
            .iter()
            .find(|declaration| declaration.name == function_name)
            .and_then(|declaration| declaration.behavior.clone())
    })
}
