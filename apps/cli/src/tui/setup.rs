//! Setting up the video tools from the terminal: a card that says what will
//! be downloaded, from where and under which licence, asks, then shows the
//! download. After it succeeds, a video link that was waiting for the tools
//! is looked at again.

use super::review::Draft;
use crate::tools::{self, Progress};
use fetchpath_core::CancellationToken;
use fetchpath_protocol::ProtocolError;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

pub enum Stage {
    Asking,
    Running {
        progress: Receiver<Progress>,
        step: Option<(String, CancellationToken)>,
        cancelled: bool,
    },
}

/// What a key did to the setup card.
#[derive(Debug, Eq, PartialEq)]
pub enum Answer {
    None,
    Start,
    Cancel,
}

pub struct Setup {
    pub dir: PathBuf,
    pub stage: Stage,
    /// A video link to look at again once the tools are ready.
    pub then: Option<Draft>,
}

impl Setup {
    pub fn new(then: Option<Draft>) -> Result<Self, ProtocolError> {
        Ok(Self {
            dir: tools::install_dir()?,
            stage: Stage::Asking,
            then,
        })
    }

    pub fn is_running(&self) -> bool {
        matches!(self.stage, Stage::Running { .. })
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Answer {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if key.kind == KeyEventKind::Release {
            return Answer::None;
        }
        let cancel = key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL));
        match &mut self.stage {
            Stage::Asking if key.code == KeyCode::Enter => Answer::Start,
            Stage::Asking if cancel => Answer::Cancel,
            // Cancelling a download in progress stops it; the result arrives
            // through `poll` like any other ending.
            Stage::Running {
                step, cancelled, ..
            } if cancel => {
                if let Some((_, token)) = step {
                    token.cancel();
                }
                *cancelled = true;
                Answer::None
            }
            _ => Answer::None,
        }
    }

    pub fn start(&mut self) -> Result<(), ProtocolError> {
        self.stage = Stage::Running {
            progress: tools::install_in_background()?,
            step: None,
            cancelled: false,
        };
        Ok(())
    }

    /// Takes progress; returns the outcome once the install has ended.
    pub fn poll(&mut self) -> Option<Result<Vec<String>, String>> {
        let Stage::Running {
            progress,
            step,
            cancelled,
        } = &mut self.stage
        else {
            return None;
        };
        loop {
            match progress.try_recv() {
                Ok(Progress::Step { label, token }) => {
                    if *cancelled {
                        token.cancel();
                    }
                    *step = Some((label, token));
                }
                Ok(Progress::Done(result)) => {
                    return Some(if *cancelled && result.is_err() {
                        Err("Setup cancelled; nothing was installed.".to_owned())
                    } else {
                        result
                    });
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    return Some(Err("The setup stopped unexpectedly.".to_owned()));
                }
            }
        }
    }

    /// The current download, its bytes and stated size.
    pub fn current(&self) -> Option<(&str, u64, Option<u64>)> {
        match &self.stage {
            Stage::Running {
                step: Some((label, token)),
                ..
            } => Some((label.as_str(), token.received(), token.total())),
            _ => None,
        }
    }
}
