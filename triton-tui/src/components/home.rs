use color_eyre::eyre::Result;
use ratatui::prelude::*;
use ratatui::widgets::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::config::Config;

use super::Component;
use super::Frame;

#[derive(Default)]
pub(crate) struct Home {
    command_tx: Option<UnboundedSender<Action>>,
    config: Config,
}

impl Home {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Component for Home {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.command_tx = Some(tx);
        Ok(())
    }

    fn register_config_handler(&mut self, config: Config) -> Result<()> {
        self.config = config;
        Ok(())
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::Tick => {}
            Action::Refresh => {}
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        f.render_widget(Paragraph::new("hello world"), area);
        Ok(())
    }
}
