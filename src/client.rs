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

use std::fmt::{Display, Formatter};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::join;
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[derive(Deserialize, Debug)]
struct LoginResponse {
    #[serde(rename = "token")]
    token: String,
}

#[derive(Deserialize, Debug)]
struct MembershipResponse {
    #[serde(rename = "id")]
    id: String,
    #[serde(rename = "shortId")]
    short_id: String,
    #[serde(rename = "name")]
    name: String,
}

struct WsConnectMessage {
    token: String,
}

impl WsConnectMessage {
    fn new(token: &str) -> WsConnectMessage {
        WsConnectMessage {
            token: token.to_string(),
        }
    }
}

impl Display for WsConnectMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = format!(
            "CONNECT\ntoken:{}\naccept-version:1.2,1.1,1.0\nheart-beat:20000,0\n\n\0",
            self.token
        );
        write!(f, "{}", str)
    }
}

struct WsSubscribeMessage {
    room_id: String,
}

impl WsSubscribeMessage {
    fn new(room_id: &str) -> WsSubscribeMessage {
        WsSubscribeMessage {
            room_id: room_id.to_string(),
        }
    }
}

#[derive(Debug)]
struct WsFeedbackMessage {
    body: WsFeedbackBody,
}

impl WsFeedbackMessage {
    fn parse(raw: &str) -> Result<WsFeedbackMessage, ()> {
        let parts = raw.split("\n\n");
        match serde_json::from_str::<WsFeedbackBody>(parts.last().unwrap().replace('\0', "").trim())
        {
            Ok(body) => Ok(WsFeedbackMessage { body }),
            Err(_) => Err(()),
        }
    }
}

#[derive(Deserialize, Debug)]
struct WsFeedbackBody {
    #[serde(rename = "type")]
    body_type: String,
    payload: WsFeedbackPayload,
}

#[derive(Deserialize, Debug)]
struct WsFeedbackPayload {
    values: Vec<u16>,
}

impl WsFeedbackPayload {
    fn get_feedback(self) -> Feedback {
        Feedback::from_values(self.values)
    }
}

impl Display for WsSubscribeMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = format!(
            "SUBSCRIBE\nid:sub-6\ndestination:/topic/{}.feedback.stream\n\n\0",
            self.room_id
        );
        write!(f, "{}", str)
    }
}

#[derive(Debug)]
pub struct RoomInfo {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct Feedback {
    pub very_good: u16,
    pub good: u16,
    pub bad: u16,
    pub very_bad: u16,
}

impl Feedback {
    pub fn from_values(values: Vec<u16>) -> Feedback {
        let mut result = Feedback {
            very_good: 0,
            good: 0,
            bad: 0,
            very_bad: 0,
        };

        if !values.is_empty() {
            result.very_good = values[0]
        }
        if values.len() >= 2 {
            result.good = values[1]
        }
        if values.len() >= 3 {
            result.bad = values[2]
        }
        if values.len() >= 4 {
            result.very_bad = values[3]
        }

        result
    }

    pub fn count_votes(&self) -> u16 {
        self.very_good + self.good + self.bad + self.very_bad
    }
}

#[allow(dead_code)]
pub enum FeedbackHandler {
    Fn(fn(&Feedback)),
    Sender(Sender<Feedback>),
}

pub struct Client {
    api_url: String,
    http_client: reqwest::Client,
    token: Option<String>,
}

impl Client {
    pub fn new(api_url: &str) -> Result<Client, ()> {
        let client = reqwest::Client::builder()
            .user_agent(format!("arsnova-cli-client/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ())?;

        Ok(Client {
            api_url: api_url.to_string(),
            http_client: client,
            token: None,
        })
    }

    pub async fn guest_login(&mut self) -> Result<(), ()> {
        match self
            .http_client
            .post(format!("{}/auth/login/guest", self.api_url))
            .send()
            .await
        {
            Ok(res) => match res.json::<LoginResponse>().await {
                Ok(res) => {
                    self.token = Some(res.token);
                    Ok(())
                }
                Err(_) => Err(()),
            },
            Err(_) => Err(()),
        }
    }

    pub async fn get_room_info(&self, short_id: &str) -> Result<RoomInfo, ()> {
        let token = self.token.as_ref().unwrap();

        let membership_response = match self
            .http_client
            .post(format!(
                "{}/room/~{}/request-membership",
                self.api_url, short_id
            ))
            .bearer_auth(token.to_string())
            .header("ars-room-role", "PARTICIPANT")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(res) => res.json::<MembershipResponse>().await.unwrap(),
            Err(err) => {
                eprintln!("{}", err);
                return Err(());
            }
        };

        Ok(RoomInfo {
            id: membership_response.id,
            short_id: membership_response.short_id,
            name: membership_response.name,
            description: String::new(),
        })
    }

    pub async fn get_feedback(&self, short_id: &str) -> Result<Feedback, ()> {
        let room_info = self.get_room_info(short_id).await?;

        let res = self
            .http_client
            .get(format!("{}/room/{}/survey", self.api_url, room_info.id))
            .bearer_auth(self.token.as_ref().unwrap_or(&"".to_string()).to_string())
            .send()
            .await
            .map_err(|_| ())?;

        Ok(Feedback::from_values(res.json::<Vec<u16>>().await.unwrap()))
    }

    pub async fn on_feedback_changed(
        &self,
        short_id: &str,
        handler: FeedbackHandler,
    ) -> Result<(), ()> {
        let room_info = self.get_room_info(short_id).await?;

        let ws_url = self.api_url.replace("http", "ws");
        let (socket, _) = connect_async(Url::parse(&format!("{}/ws/websocket", ws_url)).unwrap())
            .await
            .map_err(|_| ())?;

        let (mut write, read) = socket.split();

        if write
            .send(Message::Text(
                WsConnectMessage::new(self.token.as_ref().unwrap()).to_string(),
            ))
            .await
            .is_ok()
        {
            match write
                .send(Message::Text(
                    WsSubscribeMessage::new(&room_info.id).to_string(),
                ))
                .await
            {
                Ok(_) => {}
                Err(_) => return Err(()),
            }

            let jh1 = read.for_each(|msg| async {
                if let Ok(msg) = msg {
                    if msg.is_text() && msg.clone().into_text().unwrap().starts_with("MESSAGE") {
                        if let Ok(msg) = WsFeedbackMessage::parse(msg.to_text().unwrap()) {
                            if msg.body.body_type == "FeedbackChanged" {
                                let feedback = msg.body.payload.get_feedback();
                                match &handler {
                                    FeedbackHandler::Fn(f) => f(&feedback),
                                    FeedbackHandler::Sender(tx) => {
                                        let _ = tx.send(feedback).await;
                                    }
                                };
                            }
                        }
                    }
                }
            });

            let jh2 = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    let _ = write.send(Message::Text("\n".to_string())).await;
                }
            });

            let _ = join!(jh1, jh2);
            return Ok(());
        }

        Err(())
    }
}
