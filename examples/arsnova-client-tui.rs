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

use arsnova_client::{Client, Feedback, FeedbackHandler, FeedbackValue};

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

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), ()> {
    let cli = Cli::parse();

    let client = match Client::new(&cli.url) {
        Ok(client) => client,
        Err(_) => return Err(()),
    };

    let client = client.guest_login().await.map_err(|_| ())?;

    let (in_tx, in_rx) = channel::<Feedback>(10);
    let (out_tx, out_rx) = channel::<FeedbackValue>(10);

    let _ = in_tx
        .clone()
        .send(client.get_feedback(&cli.room).await.unwrap())
        .await;

    stdout().execute(EnterAlternateScreen).map_err(|_| ())?;
    enable_raw_mode().map_err(|_| ())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(|_| ())?;
    terminal.clear().map_err(|_| ())?;

    let l1 = client.on_feedback_changed(&cli.room, FeedbackHandler::SenderReceiver(in_tx, out_rx));

    let room_info = client.get_room_info(&cli.room).await.map_err(|_| ())?;
    let title = format!("Live Feedback: {} ({})", room_info.name, room_info.short_id);

    let l2 = create_ui(&mut terminal, &title, in_rx);

    let l3 = tokio::spawn(async move {
        loop {
            if event::poll(std::time::Duration::from_millis(16))
                .map_err(|_| ())
                .is_ok()
            {
                if let event::Event::Key(key) = event::read().map_err(|_| ()).unwrap() {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Esc => break,
                            KeyCode::Char('a') | KeyCode::Char('1') => {
                                let _ = out_tx.send(FeedbackValue::VeryGood).await;
                            }
                            KeyCode::Char('b') | KeyCode::Char('2') => {
                                let _ = out_tx.send(FeedbackValue::Good).await;
                            }
                            KeyCode::Char('c') | KeyCode::Char('3') => {
                                let _ = out_tx.send(FeedbackValue::Bad).await;
                            }
                            KeyCode::Char('d') | KeyCode::Char('4') => {
                                let _ = out_tx.send(FeedbackValue::VeryBad).await;
                            }
                            _ => {}
                        };
                    }
                }
            }
        }
    });

    select! {
        _ = l1 => {},
        _ = l2 => {},
        _ = l3 => {}
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
    const ICONS: [&str; 4] = ["Super", "Gut", "Nicht so gut", "Schlecht"];

    let feedback_paragraph =
        |feedback: &Feedback, idx: usize, width: usize| -> Paragraph<'static> {
            let value = match idx {
                0 => feedback.very_good,
                1 => feedback.good,
                2 => feedback.bad,
                3 => feedback.very_bad,
                _ => 0,
            };

            let icons = ICONS
                .iter()
                .map(|icon| format!("{: <12}", icon))
                .collect::<Vec<_>>();

            let icon = match idx {
                0 => &icons[0],
                1 => &icons[1],
                2 => &icons[2],
                3 => &icons[3],
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
        };

    loop {
        let feedback = match rx.recv().await {
            Some(feedback) => feedback,
            _ => continue,
        };

        let _ = terminal.draw(|frame| {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Max(1),
                    Constraint::Max(6),
                    Constraint::Max(2),
                    Constraint::Max(1),
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
                layout[5],
            );

            let feedback_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
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

            let button_layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(25),
                    Constraint::Percentage(25),
                    Constraint::Percentage(25),
                    Constraint::Percentage(25),
                    Constraint::Min(0),
                ])
                .split(layout[3]);

            ICONS.iter().enumerate().for_each(|(idx, label)| {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw(format!(" {} ", idx + 1))
                            .white()
                            .on_magenta()
                            .bold(),
                        Span::raw(format!("{: ^14}", label)).white().on_black(),
                    ]))
                    .alignment(Alignment::Center),
                    button_layout[idx],
                )
            });
        });
    }
}
