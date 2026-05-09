//! Execution options and CLI argument building.

use std::path::PathBuf;

/// Options for gemini exec.
#[derive(Debug, Clone, Default)]
pub struct GeminiOptions {
    pub model: Option<String>,
    pub timeout: Option<std::time::Duration>,
    pub working_dir: Option<PathBuf>,
    pub ephemeral: bool,
    /// Pass `--yolo` for headless non-interactive use (auto-approves all actions).
    pub yolo: bool,
}

/// Options for resuming a gemini session.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub working_dir: Option<PathBuf>,
    /// Pass `--yolo` for headless non-interactive use (auto-approves all actions).
    pub yolo: bool,
}

impl GeminiOptions {
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(ref model) = self.model {
            args.extend(["-m".to_owned(), model.clone()]);
        }
        if self.ephemeral {
            args.push("--ephemeral".to_owned());
        }
        if self.yolo {
            args.push("--yolo".to_owned());
        }

        args
    }
}

impl SessionOptions {
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(ref id) = self.session_id {
            args.extend(["--resume".to_owned(), id.clone()]);
        }
        if let Some(ref model) = self.model {
            args.extend(["-m".to_owned(), model.clone()]);
        }
        if self.yolo {
            args.push("--yolo".to_owned());
        }

        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_options() {
        let opts = GeminiOptions::default();
        assert!(opts.to_cli_args().is_empty());
    }

    #[test]
    fn test_model_option() {
        let opts = GeminiOptions {
            model: Some("gemini-2.0".into()),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"-m".to_owned()));
        assert!(args.contains(&"gemini-2.0".to_owned()));
    }

    #[test]
    fn test_ephemeral_option() {
        let opts = GeminiOptions {
            ephemeral: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--ephemeral".to_owned()));
    }

    #[test]
    fn test_yolo_option() {
        let opts = GeminiOptions {
            yolo: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--yolo".to_owned()));
    }

    #[test]
    fn test_session_yolo_option() {
        let opts = SessionOptions {
            yolo: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--yolo".to_owned()));
    }

    #[test]
    fn test_all_options() {
        let opts = GeminiOptions {
            model: Some("gemini-2.0".into()),
            ephemeral: true,
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn test_session_options_resume() {
        let opts = SessionOptions {
            session_id: Some("sess_123".into()),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.contains(&"--resume".to_owned()));
        assert!(args.contains(&"sess_123".to_owned()));
    }

    #[test]
    fn test_session_options_with_model() {
        let opts = SessionOptions {
            session_id: Some("sess_456".into()),
            model: Some("gemini-flash".into()),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(args.len() >= 4);
    }

    #[test]
    fn test_working_dir_not_in_args() {
        let opts = GeminiOptions {
            working_dir: Some(PathBuf::from("/tmp")),
            ..Default::default()
        };
        let args = opts.to_cli_args();
        assert!(!args.contains(&"/tmp".to_owned()));
    }
}
