use std::path::PathBuf;

use devo_protocol::InputItem;
use devo_protocol::SessionId;
use devo_protocol::TurnId;
use devo_protocol::TurnStartParams;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum InputHistoryDirection {
    Previous,
    Next,
}

/// Command requests emitted by v2 UI components.
///
/// Codex keeps this as a thin wrapper around its protocol-wide `Op` enum. Claw's
/// protocol is RPC-shaped instead, so the TUI owns a small command enum and the
/// host/worker adapter converts the relevant variants into protocol params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum AppCommand {
    RunUserShellCommand {
        command: String,
    },
    Compact,
    UserTurn {
        input: Vec<InputItem>,
        cwd: Option<PathBuf>,
        model: Option<String>,
        thinking: Option<String>,
        sandbox: Option<String>,
        approval_policy: Option<String>,
    },
    OverrideTurnContext {
        cwd: Option<PathBuf>,
        model: Option<String>,
        thinking: Option<Option<String>>,
        sandbox: Option<Option<String>>,
        approval_policy: Option<Option<String>>,
    },
    SteerTurn {
        input: Vec<InputItem>,
        expected_turn_id: TurnId,
    },
    BrowseInputHistory {
        direction: InputHistoryDirection,
    },
    SwitchSession {
        session_id: SessionId,
    },
}

#[allow(dead_code)]
pub(crate) enum AppCommandView<'a> {
    Interrupt {
        reason: &'a Option<String>,
    },
    CleanBackgroundTerminals,
    RunUserShellCommand {
        command: &'a str,
    },
    Compact,
    UserTurn {
        input: &'a [InputItem],
        cwd: &'a Option<PathBuf>,
        model: &'a Option<String>,
        thinking: &'a Option<String>,
        sandbox: &'a Option<String>,
        approval_policy: &'a Option<String>,
    },
    SteerTurn {
        input: &'a [InputItem],
    },
    OverrideTurnContext {
        cwd: &'a Option<PathBuf>,
        model: &'a Option<String>,
        thinking: &'a Option<Option<String>>,
        sandbox: &'a Option<Option<String>>,
        approval_policy: &'a Option<Option<String>>,
    },
    ReloadUserConfig,
    ListSkills {
        cwds: &'a [PathBuf],
        force_reload: bool,
    },
    SetThreadName {
        name: &'a str,
    },
    Shutdown,
    ThreadRollback {
        num_turns: u32,
    },
    Review {
        request: &'a str,
    },
    BrowseInputHistory {
        direction: InputHistoryDirection,
    },
    SwitchSession {
        session_id: SessionId,
    },
}

impl AppCommand {
    #[allow(dead_code)]
    pub(crate) fn run_user_shell_command(command: String) -> Self {
        Self::RunUserShellCommand { command }
    }

    pub(crate) fn user_turn(
        input: Vec<InputItem>,
        cwd: Option<PathBuf>,
        model: Option<String>,
        thinking: Option<String>,
        sandbox: Option<String>,
        approval_policy: Option<String>,
    ) -> Self {
        Self::UserTurn {
            input,
            cwd,
            model,
            thinking,
            sandbox,
            approval_policy,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn text_turn(text: String, cwd: Option<PathBuf>, model: Option<String>) -> Self {
        Self::user_turn(
            vec![InputItem::Text { text }],
            cwd,
            model,
            /*thinking*/ None,
            /*sandbox*/ None,
            /*approval_policy*/ None,
        )
    }

    pub(crate) fn override_turn_context(
        cwd: Option<PathBuf>,
        model: Option<String>,
        thinking: Option<Option<String>>,
        sandbox: Option<Option<String>>,
        approval_policy: Option<Option<String>>,
    ) -> Self {
        Self::OverrideTurnContext {
            cwd,
            model,
            thinking,
            sandbox,
            approval_policy,
        }
    }

    pub(crate) fn browse_input_history(direction: InputHistoryDirection) -> Self {
        Self::BrowseInputHistory { direction }
    }

    pub(crate) fn compact() -> Self {
        Self::Compact
    }

    pub(crate) fn switch_session(session_id: SessionId) -> Self {
        Self::SwitchSession { session_id }
    }

    #[allow(dead_code)]
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::RunUserShellCommand { .. } => "run_user_shell_command",
            Self::Compact => "compact",
            Self::UserTurn { .. } => "user_turn",
            Self::OverrideTurnContext { .. } => "override_turn_context",
            Self::SteerTurn { .. } => "steer_turn",
            Self::BrowseInputHistory { .. } => "browse_input_history",
            Self::SwitchSession { .. } => "switch_session",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn view(&self) -> AppCommandView<'_> {
        match self {
            Self::RunUserShellCommand { command } => {
                AppCommandView::RunUserShellCommand { command }
            }
            Self::Compact => AppCommandView::Compact,
            Self::UserTurn {
                input,
                cwd,
                model,
                thinking,
                sandbox,
                approval_policy,
            } => AppCommandView::UserTurn {
                input,
                cwd,
                model,
                thinking,
                sandbox,
                approval_policy,
            },
            Self::OverrideTurnContext {
                cwd,
                model,
                thinking,
                sandbox,
                approval_policy,
            } => AppCommandView::OverrideTurnContext {
                cwd,
                model,
                thinking,
                sandbox,
                approval_policy,
            },
            Self::SteerTurn { input, .. } => AppCommandView::SteerTurn { input },
            Self::BrowseInputHistory { direction } => AppCommandView::BrowseInputHistory {
                direction: *direction,
            },
            Self::SwitchSession { session_id } => AppCommandView::SwitchSession {
                session_id: *session_id,
            },
        }
    }

    #[allow(dead_code)]
    pub(crate) fn to_turn_start_params(&self, session_id: SessionId) -> Option<TurnStartParams> {
        let Self::UserTurn {
            input,
            cwd,
            model,
            thinking,
            sandbox,
            approval_policy,
        } = self
        else {
            return None;
        };

        Some(TurnStartParams {
            session_id,
            input: input.clone(),
            model: model.clone(),
            thinking: thinking.clone(),
            sandbox: sandbox.clone(),
            approval_policy: approval_policy.clone(),
            cwd: cwd.clone(),
        })
    }
}
