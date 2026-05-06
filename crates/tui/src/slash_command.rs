/// Commands that can be invoked by starting a message with a leading slash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlashCommand {
    Theme,
    Model,
    Compact,
    Thinking,
    Resume,
    New,
    Status,
    Clear,
    Onboard,
    Diff,
    Exit,
    Btw,
}

impl SlashCommand {
    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::Theme => "switch the UI theme",
            SlashCommand::Model => "choose the active model",
            SlashCommand::Compact => "compact the current session context",
            SlashCommand::Thinking => "choose the active thinking mode",
            SlashCommand::Resume => "resume a saved chat",
            SlashCommand::New => "start a new chat",
            SlashCommand::Status => "show current session configuration and token usage",
            SlashCommand::Clear => "clear the current transcript",
            SlashCommand::Onboard => "configure model provider connection",
            SlashCommand::Diff => "show git diff (including untracked files)",
            SlashCommand::Btw => "inject text into the current turn immediately",
            SlashCommand::Exit => "exit Devo",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            SlashCommand::Theme => "theme",
            SlashCommand::Model => "model",
            SlashCommand::Compact => "compact",
            SlashCommand::Thinking => "thinking",
            SlashCommand::Resume => "resume",
            SlashCommand::New => "new",
            SlashCommand::Status => "status",
            SlashCommand::Clear => "clear",
            SlashCommand::Onboard => "onboard",
            SlashCommand::Diff => "diff",
            SlashCommand::Btw => "btw",
            SlashCommand::Exit => "exit",
        }
    }

    pub fn supports_inline_args(self) -> bool {
        matches!(self, SlashCommand::Model | SlashCommand::Btw)
    }

    pub fn available_during_task(self) -> bool {
        !matches!(
            self,
            SlashCommand::Model
                | SlashCommand::Theme
                | SlashCommand::Thinking
                | SlashCommand::Onboard
                | SlashCommand::Compact
                | SlashCommand::Diff
                | SlashCommand::New
                | SlashCommand::Resume
        )
    }
}

impl std::str::FromStr for SlashCommand {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "theme" => Ok(Self::Theme),
            "model" => Ok(Self::Model),
            "compact" => Ok(Self::Compact),
            "thinking" => Ok(Self::Thinking),
            "resume" => Ok(Self::Resume),
            "new" => Ok(Self::New),
            "status" => Ok(Self::Status),
            "clear" => Ok(Self::Clear),
            "onboard" => Ok(Self::Onboard),
            "diff" => Ok(Self::Diff),
            "btw" => Ok(Self::Btw),
            "exit" => Ok(Self::Exit),
            _ => Err(()),
        }
    }
}

pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    vec![
        ("theme", SlashCommand::Theme),
        ("model", SlashCommand::Model),
        ("compact", SlashCommand::Compact),
        ("thinking", SlashCommand::Thinking),
        ("resume", SlashCommand::Resume),
        ("new", SlashCommand::New),
        ("status", SlashCommand::Status),
        ("clear", SlashCommand::Clear),
        ("onboard", SlashCommand::Onboard),
        ("diff", SlashCommand::Diff),
        ("btw", SlashCommand::Btw),
        ("exit", SlashCommand::Exit),
    ]
}
