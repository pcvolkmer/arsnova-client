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

use std::error;
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use reqwest::{IntoUrl, StatusCode};
use serde::Deserialize;
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::client::ClientError::{
    ConnectionError, LoginError, ParserError, RoomNotFoundError, UrlError,
};

#[derive(Deserialize, Debug)]
struct LoginResponse {
    #[serde(rename = "token")]
    token: String,
}

#[derive(Deserialize, Debug)]
struct TokenClaim {
    sub: String,
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
    values: [u16; 4],
}

impl WsFeedbackPayload {
    fn get_feedback(self) -> Feedback {
        Feedback::from_values(self.values)
    }
}

#[derive(Debug)]
struct WsCreateFeedbackMessage {
    room_id: String,
    user_id: String,
    value: u8,
}

impl WsCreateFeedbackMessage {
    fn new(room_id: &str, user_id: &str, value: FeedbackValue) -> WsCreateFeedbackMessage {
        WsCreateFeedbackMessage {
            room_id: room_id.into(),
            user_id: user_id.into(),
            value: value.into_u8(),
        }
    }
}

impl Display for WsCreateFeedbackMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let payload = json!({
            "type": "CreateFeedback",
            "payload": {
                "roomId": self.room_id,
                "userId": self.user_id,
                "value": self.value
            }
        })
        .to_string();

        write!(f,
                "SEND\ndestination:/queue/feedback.command\ncontent-type:application/json\ncontent-length:{}\n\n{}\0",
                payload.chars().count(),
                payload,
            )
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

#[derive(Deserialize, Clone, Debug)]
pub struct SummaryResponse {
    pub stats: RoomStats,
}

#[derive(Deserialize, Clone, Debug)]
pub struct RoomStats {
    #[serde(rename = "contentCount")]
    pub content_count: usize,
    #[serde(rename = "ackCommentCount")]
    pub ack_comment_count: usize,
    #[serde(rename = "roomUserCount")]
    pub room_user_count: usize,
}

#[derive(Debug, Clone)]
pub struct Feedback {
    pub very_good: u16,
    pub good: u16,
    pub bad: u16,
    pub very_bad: u16,
}

impl Feedback {
    pub fn from_values(values: [u16; 4]) -> Feedback {
        Feedback {
            very_good: values[0],
            good: values[1],
            bad: values[2],
            very_bad: values[3],
        }
    }

    pub fn count_votes(&self) -> u16 {
        self.very_good + self.good + self.bad + self.very_bad
    }
}

#[allow(dead_code)]
pub enum FeedbackHandler {
    /// Handle incoming `Feedback` using a fn
    Fn(fn(&Feedback)),
    /// Handle incoming `Feedback` by sending it to a `Sender<Feedback>`
    Sender(Sender<Feedback>),
    /// Bidirectional handler for incoming `Feedback` and outgoing `FeedbackValue`
    SenderReceiver(Sender<Feedback>, Receiver<FeedbackValue>),
}

/// A possible feedback value
#[derive(Clone)]
pub enum FeedbackValue {
    VeryGood,
    A,
    Good,
    B,
    Bad,
    C,
    VeryBad,
    D,
}

impl FeedbackValue {
    /// Returns internal u8 representation
    fn into_u8(self) -> u8 {
        match self {
            FeedbackValue::VeryGood | FeedbackValue::A => 0,
            FeedbackValue::Good | FeedbackValue::B => 1,
            FeedbackValue::Bad | FeedbackValue::C => 2,
            FeedbackValue::VeryBad | FeedbackValue::D => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    ConnectionError,
    LoginError,
    RoomNotFoundError(String),
    ParserError(String),
    UrlError,
}

impl Display for ClientError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError => write!(f, "Cannot connect"),
            LoginError => write!(f, "Cannot login"),
            RoomNotFoundError(short_id) => write!(f, "Requested room '{}' not found", short_id),
            ParserError(msg) => write!(f, "Cannot parse response: {}", msg),
            UrlError => write!(f, "Cannot parse given URL"),
        }
    }
}

impl error::Error for ClientError {}

pub struct LoggedIn;
pub struct LoggedOut;

/// An asynchronous `Client` to make Requests with.
///
/// The client can be created with an URL to an ARSnova API endpoint.
pub struct Client<State = LoggedOut> {
    api_url: String,
    http_client: reqwest::Client,
    token: Option<String>,
    state: PhantomData<State>,
}

impl Client {
    /// Constructs a new ARSnova client
    ///
    /// This method fails whenever the supplied Url cannot be parsed.
    ///
    /// If successful the result will be of type `Client<LoggedOut>`
    pub fn new<U: IntoUrl>(api_url: U) -> Result<Client, ClientError> {
        let client = reqwest::Client::builder()
            .user_agent(format!("arsnova-cli-client/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ConnectionError)?;

        Ok(Client {
            api_url: api_url.into_url().map_err(|_| UrlError)?.to_string(),
            http_client: client,
            token: None,
            state: PhantomData::<LoggedOut>,
        })
    }
}

impl Client<LoggedOut> {
    /// Tries to login and request a new token if client is not logged in yet
    ///
    /// This method fails if a connection error occurs or the response cannot
    /// be handled.
    ///
    /// If successful the result will be of type `Client<LoggedIn>`
    pub async fn guest_login(self) -> Result<Client<LoggedIn>, ClientError> {
        match self
            .http_client
            .post(format!("{}/auth/login/guest", self.api_url))
            .send()
            .await
        {
            Ok(res) => match res.json::<LoginResponse>().await {
                Ok(res) => Ok(Client {
                    api_url: self.api_url,
                    http_client: self.http_client,
                    token: Some(res.token),
                    state: PhantomData::<LoggedIn>,
                }),
                Err(_) => Err(LoginError),
            },
            Err(_) => Err(ConnectionError),
        }
    }
}

impl Client<LoggedIn> {
    /// Get user ID extracted from client token
    ///
    /// This method fails if the token cannot be parsed
    pub fn get_user_id(&self) -> Result<String, ClientError> {
        let token = self.token.clone().unwrap_or_default();
        let mut token_parts = token.split('.');

        match token_parts.nth(1) {
            None => Err(ParserError("Unparsable token".into())),
            Some(part) => match STANDARD_NO_PAD.decode(part) {
                Ok(d) => match serde_json::from_str::<TokenClaim>(
                    &String::from_utf8(d).unwrap_or_default(),
                ) {
                    Ok(claim) => Ok(claim.sub),
                    Err(err) => Err(ParserError(format!("Unparsable token claim: {}", err))),
                },
                Err(err) => Err(ParserError(format!("Unparsable token: {}", err))),
            },
        }
    }

    /// Logout the client and discard existing token if not logged in
    ///
    /// If successful the result will be of type `Client<LoggedOut>`
    pub fn logout(self) -> Client<LoggedOut> {
        Client {
            api_url: self.api_url,
            http_client: self.http_client,
            token: None,
            state: PhantomData::<LoggedOut>,
        }
    }

    /// Requests `RoomInfo` for given 8-digit room ID
    ///
    /// This method fails on connection or response errors and if
    /// no room is available with given room ID.
    pub async fn get_room_info(&self, short_id: &str) -> Result<RoomInfo, ClientError> {
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
            Ok(res) => match res.status() {
                StatusCode::OK => res
                    .json::<MembershipResponse>()
                    .await
                    .map_err(|err| ParserError(err.to_string()))?,
                StatusCode::NOT_FOUND => return Err(RoomNotFoundError(short_id.into())),
                _ => return Err(ConnectionError),
            },
            Err(_) => {
                return Err(ConnectionError);
            }
        };

        Ok(RoomInfo {
            id: membership_response.id,
            short_id: membership_response.short_id,
            name: membership_response.name,
            description: String::new(),
        })
    }

    /// Requests `Feedback` for given 8-digit room ID
    ///
    /// This method fails on connection or response errors and if
    /// no room is available with given room ID.
    pub async fn get_feedback(&self, short_id: &str) -> Result<Feedback, ClientError> {
        let room_info = self.get_room_info(short_id).await?;

        match self
            .http_client
            .get(format!("{}/room/{}/survey", self.api_url, room_info.id))
            .bearer_auth(self.token.as_ref().unwrap_or(&"".to_string()).to_string())
            .send()
            .await
        {
            Ok(res) => match res.status() {
                StatusCode::OK => Ok(Feedback::from_values(
                    res.json::<[u16; 4]>()
                        .await
                        .map_err(|err| ParserError(err.to_string()))?,
                )),
                StatusCode::NOT_FOUND => Err(RoomNotFoundError(short_id.into())),
                _ => Err(ConnectionError),
            },
            Err(_) => Err(ConnectionError),
        }
    }

    /// Requests `RoomStats` for given 8-digit room ID
    ///
    /// This method fails on connection or response errors and if
    /// no room is available with given room ID.
    pub async fn get_room_stats(&self, short_id: &str) -> Result<RoomStats, ClientError> {
        let room_info = self.get_room_info(short_id).await?;

        match self
            .http_client
            .get(format!(
                "{}/_view/room/summary?ids={}",
                self.api_url, room_info.id
            ))
            .bearer_auth(self.token.as_ref().unwrap_or(&"".to_string()).to_string())
            .send()
            .await
        {
            Ok(res) => match res.status() {
                StatusCode::OK => Ok(res
                    .json::<Vec<SummaryResponse>>()
                    .await
                    .map_err(|err| ParserError(err.to_string()))
                    .map(|summary_response| summary_response.first().unwrap().stats.clone()))?,
                StatusCode::NOT_FOUND => Err(RoomNotFoundError(short_id.into())),
                _ => Err(ConnectionError),
            },
            Err(_) => Err(ConnectionError),
        }
    }

    /// Register feedback channel receiver and send incoming feedback to service
    ///
    /// This method fails on connection or response errors and if
    /// no room is available with given room ID.
    pub async fn register_feedback_receiver(
        &self,
        short_id: &str,
        mut receiver: Receiver<FeedbackValue>,
    ) -> Result<(), ClientError> {
        let room_info = self.get_room_info(short_id).await?;

        let ws_url = self.api_url.replace("http", "ws");
        let (socket, _) = connect_async(Url::parse(&format!("{}/ws/websocket", ws_url)).unwrap())
            .await
            .map_err(|_| ConnectionError)?;

        let (mut write, _) = socket.split();

        let user_id = self.get_user_id().unwrap_or_default();

        if write
            .send(Message::Text(
                WsConnectMessage::new(self.token.as_ref().unwrap()).to_string(),
            ))
            .await
            .is_ok()
        {
            return match write
                .send(Message::Text(
                    WsSubscribeMessage::new(&room_info.id).to_string(),
                ))
                .await
            {
                Ok(_) => loop {
                    select!(
                        Some(value) = receiver.recv() =>
                        {
                            let msg = WsCreateFeedbackMessage::new(&room_info.id, &user_id, value.to_owned()).to_string();
                            let _ = write.send(Message::Text(msg)).await;
                        },
                        _ = tokio::time::sleep(Duration::from_secs(15)) => {
                            let _ = write.send(Message::Text("\n".to_string())).await;
                        }
                    )
                },
                Err(_) => Err(ConnectionError),
            };
        }

        Err(ConnectionError)
    }

    /// Registers a handler to get notifications on feedback change.
    ///
    /// This is done by using websocket connections to ARSnova.
    ///
    /// This method fails on connection or response errors and if
    /// no room is available with given room ID.
    pub async fn on_feedback_changed(
        &self,
        short_id: &str,
        handler: FeedbackHandler,
    ) -> Result<(), ClientError> {
        let room_info = self.get_room_info(short_id).await?;

        let ws_url = self.api_url.replace("http", "ws");
        let (socket, _) = connect_async(Url::parse(&format!("{}/ws/websocket", ws_url)).unwrap())
            .await
            .map_err(|_| ConnectionError)?;

        let (mut write, mut read) = socket.split();

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
                Ok(_) => match handler {
                    FeedbackHandler::Fn(f) => loop {
                        select! {
                            Some(next) = read.next() => {
                                match &next {
                                    Ok(msg) => self.handle_incoming_feedback_with_fn(msg, &f).await,
                                    Err(_) => break
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                                let _ = write.send(Message::Text("\n".to_string())).await;
                            }
                        }
                    },
                    FeedbackHandler::Sender(tx) => loop {
                        select! {
                            Some(next) = read.next() => {
                                match &next {
                                    Ok(msg) => self.handle_incoming_feedback_with_sender(msg, &tx).await,
                                    Err(_) => break
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                                let _ = write.send(Message::Text("\n".to_string())).await;
                            }
                        }
                    },
                    FeedbackHandler::SenderReceiver(tx, mut rx) => loop {
                        select! {
                            Some(next) = read.next() => {
                                match &next {
                                    Ok(msg) => self.handle_incoming_feedback_with_sender(msg, &tx).await,
                                    Err(_) => break
                                }
                            }
                            Some(value) = rx.recv() => {
                                let user_id = self.get_user_id().unwrap_or_default();
                                let msg = WsCreateFeedbackMessage::new(&room_info.id, &user_id, value.to_owned()).to_string();
                                let _ = write.send(Message::Text(msg)).await;
                            }
                            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                                let _ = write.send(Message::Text("\n".to_string())).await;
                            }
                        }
                    },
                },
                Err(_) => return Err(ConnectionError),
            }
        }

        Err(ConnectionError)
    }

    async fn handle_incoming_feedback_with_fn(&self, msg: &Message, f: &fn(&Feedback)) {
        if msg.is_text() && msg.clone().into_text().unwrap().starts_with("MESSAGE") {
            if let Ok(msg) = WsFeedbackMessage::parse(msg.to_text().unwrap()) {
                if msg.body.body_type == "FeedbackChanged" {
                    let feedback = msg.body.payload.get_feedback();
                    f(&feedback);
                }
            }
        }
    }

    async fn handle_incoming_feedback_with_sender(&self, msg: &Message, tx: &Sender<Feedback>) {
        if msg.is_text() && msg.clone().into_text().unwrap().starts_with("MESSAGE") {
            if let Ok(msg) = WsFeedbackMessage::parse(msg.to_text().unwrap()) {
                if msg.body.body_type == "FeedbackChanged" {
                    let feedback = msg.body.payload.get_feedback();
                    let _ = tx.send(feedback).await;
                }
            }
        }
    }
}
