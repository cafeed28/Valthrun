use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        Weak,
    },
};

use futures_util::{
    Future,
    SinkExt,
    StreamExt,
};
use radar_shared::{
    create_message_channel,
    protocol::{
        C2SMessage,
        ClientEvent,
        S2CMessage,
    },
};
use rand::{
    distributions::Alphanumeric,
    Rng,
};
use tokio::{
    net::{
        TcpListener,
        ToSocketAddrs,
    },
    sync::{
        mpsc::{
            self,
            Receiver,
        },
        RwLock,
    },
    task::JoinHandle,
};
use warp::{
    filters::ws::Message,
    Filter,
};

use crate::{
    client::PubClient,
    handler::ServerCommandHandler,
    ClientState,
};

pub struct PubSession {
    pub owner_id: u32,
    pub session_id: String,
    subscriber: BTreeMap<u32, mpsc::Sender<S2CMessage>>,
}

impl PubSession {
    pub fn broadcast(&self, message: &S2CMessage) {
        for subscriber in self.subscriber.values() {
            let _ = subscriber.try_send(message.clone());
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscriber.len()
    }
}

pub struct RadarServer {
    ref_self: Weak<RwLock<RadarServer>>,
    client_id_counter: u32,

    clients: BTreeMap<u32, Arc<RwLock<PubClient>>>,
    pub_sessions: BTreeMap<String, PubSession>,

    client_acceptor: Option<JoinHandle<()>>,
    www_acceptor: Option<JoinHandle<()>>,
}

impl RadarServer {
    pub fn new() -> Arc<RwLock<Self>> {
        let mut result = Self {
            ref_self: Default::default(),
            client_id_counter: 1,

            clients: Default::default(),
            pub_sessions: Default::default(),

            client_acceptor: None,
            www_acceptor: None,
        };

        Arc::new_cyclic(|weak| {
            result.ref_self = weak.clone();
            RwLock::new(result)
        })
    }

    pub async fn listen_http(&mut self, addr: impl Into<SocketAddr>) -> anyhow::Result<()> {
        if self.www_acceptor.is_some() {
            anyhow::bail!("www already started");
        }

        let server = self.ref_self.clone();
        let routes = warp::path("subscribe")
            .and(warp::addr::remote())
            .and(warp::ws())
            .map(move |address: Option<SocketAddr>, ws: warp::ws::Ws| {
                let server = server.clone();
                ws.on_upgrade(move |socket| async move {
                    let address = match address {
                        Some(address) => address,
                        None => return,
                    };

                    let (message_tx, mut message_tx_rx) = mpsc::channel(16);
                    let (message_rx_tx, message_rx) = mpsc::channel(16);

                    {
                        let server = match server.upgrade() {
                            Some(server) => server,
                            None => {
                                log::warn!(
                                    "Accepted ws client from {}, but server gone. Dropping client.",
                                    address
                                );
                                return;
                            }
                        };

                        let mut server = server.write().await;
                        let client_fut = server
                            .register_client(
                                PubClient::new(message_tx, address.clone()),
                                message_rx,
                            )
                            .await;

                        tokio::spawn(client_fut);
                    }

                    {
                        let (mut tx, mut rx) = socket.split();

                        let rx_loop = tokio::spawn({
                            let message_rx_tx = message_rx_tx.clone();
                            async move {
                                while let Some(message) = rx.next().await {
                                    let message = match message {
                                        Ok(message) => message,
                                        Err(err) => {
                                            let _ = message_rx_tx
                                                .send(ClientEvent::RecvError(err.into()))
                                                .await;
                                            break;
                                        }
                                    };

                                    if message.is_text() {
                                        let message =
                                            match serde_json::from_slice(message.as_bytes()) {
                                                Ok(message) => message,
                                                Err(err) => {
                                                    let _ = message_rx_tx
                                                        .send(ClientEvent::RecvError(err.into()))
                                                        .await;
                                                    break;
                                                }
                                            };

                                        if let Err(err) = {
                                            message_rx_tx
                                                .send(ClientEvent::RecvMessage(message))
                                                .await
                                        } {
                                            log::warn!(
                                                "Failed to submit message to queue: {}",
                                                err
                                            );
                                        }
                                    }
                                }
                            }
                        });

                        let tx_loop = tokio::spawn({
                            let message_rx_tx = message_rx_tx.clone();
                            async move {
                                while let Some(message) = message_tx_rx.recv().await {
                                    let encoded = match serde_json::to_string(&message) {
                                        Ok(message) => message,
                                        Err(err) => {
                                            let _ = message_rx_tx
                                                .send(ClientEvent::SendError(err.into()))
                                                .await;
                                            break;
                                        }
                                    };

                                    if let Err(err) = tx.send(Message::text(encoded)).await {
                                        let _ = message_rx_tx
                                            .send(ClientEvent::SendError(err.into()))
                                            .await;
                                        break;
                                    }
                                }
                            }
                        });

                        /* await until ether the read or write loop has finished */
                        tokio::select! {
                            _ = rx_loop => {},
                            _ = tx_loop => {},
                        }
                    }
                })
            });

        let (address, future) = warp::serve(routes).try_bind_ephemeral(addr)?;
        self.www_acceptor = Some(tokio::spawn(future));

        log::info!("Started WWW server on {}", address);

        Ok(())
    }

    pub async fn unregister_client(&mut self, client_id: u32) {
        let client = match self.clients.remove(&client_id) {
            Some(client) => client,
            None => return,
        };

        let client_state = {
            let client = client.read().await;
            client.state.clone()
        };
        match client_state {
            ClientState::Publisher { session_id } => {
                /* TODO: Proper session shutdown! */
                self.pub_sessions.remove(&session_id);
            }
            ClientState::Subscriber { session_id } => {
                self.pub_session_unsubscribe(&session_id, client_id).await;
            }
            ClientState::Uninitialized => { /* Nothing to do! */ }
        };

        log::debug!("Disconnected pub client {}", client_id);
    }

    pub async fn register_client(
        &mut self,
        mut client: PubClient,
        mut rx: Receiver<ClientEvent<C2SMessage>>,
    ) -> impl Future<Output = ()> {
        let client_id = self.client_id_counter.wrapping_add(1);
        self.client_id_counter = client_id;

        log::debug!(
            "Registered new pub client from {} with client id {}",
            client.address,
            client_id
        );

        client.client_id = client_id;
        let client = Arc::new(RwLock::new(client));
        self.clients.insert(client_id, client.clone());

        let command_handler = ServerCommandHandler {
            server: self.ref_self.upgrade().expect("to be present"),
            client: client.clone(),
            client_id,
        };

        async move {
            while let Some(event) = rx.recv().await {
                match event {
                    ClientEvent::RecvMessage(command) => {
                        let result = command_handler.handle_command(command).await;
                        client.read().await.send_command(result);
                    }
                    ClientEvent::RecvError(err) => {
                        log::debug!("Client {} recv error: {}", command_handler.client_id, err);
                        break;
                    }
                    ClientEvent::SendError(err) => {
                        log::debug!("Client {} recv error: {}", command_handler.client_id, err);
                        break;
                    }
                }
            }

            command_handler
                .server
                .write()
                .await
                .unregister_client(command_handler.client_id)
                .await;
        }
    }

    pub async fn listen_client<A: ToSocketAddrs>(&mut self, addr: A) -> anyhow::Result<()> {
        if self.client_acceptor.is_some() {
            anyhow::bail!("Client server already bound");
        }

        let listener = TcpListener::bind(addr).await?;
        log::info!("Started pub client server on {}", listener.local_addr()?);

        let server = self.ref_self.clone();
        let listener_handle = tokio::spawn(async move {
            loop {
                let (socket, address) = match listener.accept().await {
                    Ok(client) => client,
                    Err(err) => {
                        log::error!("Failed to accept new client: {:#}", err);
                        break;
                    }
                };

                let (tx, rx) = create_message_channel(socket);
                let server = match server.upgrade() {
                    Some(server) => server,
                    None => {
                        log::warn!(
                            "Accepted client from {}, but server gone. Dropping client.",
                            address
                        );
                        continue;
                    }
                };

                let mut server = server.write().await;
                let client_fut = server
                    .register_client(PubClient::new(tx, address), rx)
                    .await;

                tokio::spawn(client_fut);
            }
        });

        self.client_acceptor = Some(listener_handle);
        Ok(())
    }

    pub async fn pub_session_create(&mut self, owner_id: u32) -> Option<&PubSession> {
        let owner = match self.clients.get(&owner_id) {
            Some(client) => client,
            None => return None,
        };

        let mut owner = owner.write().await;
        if !matches!(owner.state, ClientState::Uninitialized) {
            return None;
        }

        let session_id = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .map(char::from)
            .take(16)
            .collect::<String>();

        self.pub_sessions.insert(
            session_id.clone(),
            PubSession {
                owner_id,
                session_id: session_id.clone(),
                subscriber: Default::default(),
            },
        );

        owner.state = ClientState::Publisher {
            session_id: session_id.clone(),
        };
        self.pub_sessions.get(&session_id)
    }

    pub fn pub_session_find(&self, session_id: &str) -> Option<&PubSession> {
        self.pub_sessions.get(session_id)
    }

    pub async fn pub_session_unsubscribe(&mut self, session_id: &String, client_id: u32) {
        if let Some(session) = self.pub_sessions.get_mut(session_id) {
            session.subscriber.remove(&client_id);
            session.broadcast(&S2CMessage::NotifyViewCount {
                viewers: session.subscriber_count(),
            });
        }

        if let Some(client) = self.clients.get(&client_id) {
            let mut client = client.write().await;
            if let ClientState::Subscriber {
                session_id: client_session_id,
            } = &client.state
            {
                if client_session_id == session_id {
                    client.state = ClientState::Uninitialized;
                } else {
                    log::warn!(
                        "Client state indicates different session id then we unregister the client"
                    );
                }
            }
        }
    }

    pub async fn pub_session_subscribe(
        &mut self,
        session_id: &String,
        client_id: u32,
    ) -> PubSessionSubscribeResult {
        let client = match self.clients.get(&client_id) {
            Some(client) => client,
            None => return PubSessionSubscribeResult::InvalidClientId,
        };

        let mut client = client.write().await;
        if !matches!(client.state, ClientState::Uninitialized) {
            return PubSessionSubscribeResult::InvalidClientState;
        }

        let session = match self.pub_sessions.get_mut(session_id) {
            Some(session) => session,
            None => return PubSessionSubscribeResult::InvalidSessionId,
        };

        session
            .subscriber
            .insert(client.client_id, client.tx.clone());

        session.broadcast(&S2CMessage::NotifyViewCount {
            viewers: session.subscriber.len(),
        });

        client.state = ClientState::Subscriber {
            session_id: session.session_id.clone(),
        };
        PubSessionSubscribeResult::Success
    }
}

pub enum PubSessionSubscribeResult {
    Success,
    InvalidClientState,
    InvalidSessionId,
    InvalidClientId,
}
