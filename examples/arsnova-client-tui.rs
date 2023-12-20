/*
 * This file is part of arsnova-client
 *
 * Copyright (C) 2023  Paul-Christian Volkmer
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Lesser General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::io::{stdout, Stdout};

use clap::Parser;
use crossterm::event::{KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{event, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use tokio::select;
use tokio::sync::mpsc::{channel, Receiver};

use arsnova_client::{Client, Feedback, FeedbackHandler};

#[derive(Parser)]
#[command(author, version, about = "Terminal-based ARSnova live feedback client", long_about = None)]
#[command(arg_required_else_help(true))]
pub struct Cli {
    #[arg(help = "Raum")]
    room: String,
    #[arg(
        short = 'u',
        long = "url",
        help = "API-URL",
        default_value = "https://ars.particify.de/api"
    )]
    url: String,
}

#[tokio::main(worker_threads = 3)]
async fn main() -> Result<(), ()> {
    let cli = Cli::parse();

    let mut client = match Client::new(&cli.url) {
        Ok(client) => client,
        Err(_) => return Err(()),
    };

    if client.guest_login().await.is_err() {
        return Err(());
    }

    let (tx, rx) = channel::<Feedback>(10);

    let _ = tx
        .clone()
        .send(client.get_feedback(&cli.room).await.unwrap())
        .await;

    stdout().execute(EnterAlternateScreen).map_err(|_| ())?;
    enable_raw_mode().map_err(|_| ())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(|_| ())?;
    terminal.clear().map_err(|_| ())?;

    let l1 = client.on_feedback_changed(&cli.room, FeedbackHandler::Sender(tx.clone()));

    let room_info = client.get_room_info(&cli.room).await.map_err(|_| ())?;
    let title = format!("Live Feedback: {} ({})", room_info.name, room_info.short_id);

    let l2 = create_ui(&mut terminal, &title, rx);

    let l3 = tokio::spawn(async {
        loop {
            if event::poll(std::time::Duration::from_millis(16))
                .map_err(|_| ())
                .is_ok()
            {
                if let event::Event::Key(key) = event::read().map_err(|_| ()).unwrap() {
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                        break;
                    }
                }
            }
        }
    });

    select! {
        _ = l1 => {},
        _ = l2 => {},
        _ = l3 => {},
    }

    let _ = stdout().execute(LeaveAlternateScreen).map_err(|_| ());
    let _ = disable_raw_mode().map_err(|_| ()).map_err(|_| ());

    Ok(())
}

async fn create_ui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    title: &str,
    mut rx: Receiver<Feedback>,
) -> Result<(), ()> {
    fn feedback_paragraph(feedback: &Feedback, idx: usize, width: usize) -> Paragraph<'static> {
        let value = match idx {
            0 => feedback.very_good,
            1 => feedback.good,
            2 => feedback.bad,
            3 => feedback.very_bad,
            _ => 0,
        };

        let icon = match idx {
            0 => "Super       ",
            1 => "Gut         ",
            2 => "Nicht so gut",
            3 => "Schlecht    ",
            _ => "            ",
        };

        let width = width - 24;

        let l = ((value as f32 / feedback.count_votes() as f32) * width as f32) as usize;

        match idx {
            0..=3 => Paragraph::new(Line::from(vec![
                Span::raw(format!("{} : ", icon)),
                Span::raw(format!("[{: >5}] ", value)).dim(),
                Span::raw("■".to_string().repeat(l).to_string())
                    .green()
                    .on_black(),
                Span::raw(" ".to_string().repeat(width - l).to_string()).on_black(),
            ])),
            _ => Paragraph::default(),
        }
    }

    loop {
        let feedback = match rx.recv().await {
            Some(feedback) => feedback,
            _ => continue,
        };

        let _ = terminal.draw(|frame| {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(vec![
                    Constraint::Max(1),
                    Constraint::Max(6),
                    Constraint::Min(1),
                    Constraint::Max(1),
                ])
                .split(frame.size());

            frame.render_widget(
                Paragraph::new(title)
                    .white()
                    .on_blue()
                    .bold()
                    .alignment(Alignment::Center),
                layout[0],
            );

            frame.render_widget(
                Paragraph::new(format!("{} Antworten", feedback.count_votes()))
                    .white()
                    .bold()
                    .alignment(Alignment::Center),
                layout[2],
            );

            frame.render_widget(
                Paragraph::new("Beenden mit <Esc>")
                    .on_blue()
                    .alignment(Alignment::Left),
                layout[3],
            );

            let feedback_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(vec![
                    Constraint::Max(1),
                    Constraint::Max(1),
                    Constraint::Max(1),
                    Constraint::Max(1),
                ])
                .margin(1)
                .split(layout[1]);

            [0usize, 1, 2, 3].iter().for_each(|&idx| {
                frame.render_widget(
                    feedback_paragraph(&feedback, idx, feedback_layout[idx].width as usize),
                    feedback_layout[idx],
                )
            });
        });
    }
}
