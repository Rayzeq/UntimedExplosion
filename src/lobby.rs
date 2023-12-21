use crate::{
    common::{make_event, GlobalState, Protected},
    game,
    gameplay::{self, errors, Game, Lobby, PlayingPlayer, Room},
};
use rand::{
    distributions::{Alphanumeric, DistString},
    random,
};
use rocket::{
    get,
    http::{CookieJar, Status},
    request::{FromRequest, Outcome, Request},
    response::{
        stream::{Event, EventStream},
        Redirect,
    },
    routes,
    serde::Serialize,
    tokio::{
        self, select,
        sync::{
            mpsc::{unbounded_channel, UnboundedSender},
            Mutex,
        },
    },
    uri, Shutdown, State,
};
use std::{collections::HashMap, sync::Arc, time::Duration};

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
pub struct Player {
    id: <Self as gameplay::Player>::ID,
    name: String,
    ready: bool,
    #[serde(skip)]
    sender: UnboundedSender<Message>,
}

impl gameplay::Player for Player {
    type ID = u32;

    fn id(&self) -> Self::ID {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl gameplay::WaitingPlayer for Player {
    fn ready(&self) -> bool {
        self.ready
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
#[serde(untagged)]
enum Message {
    SelfLeave,
    Error {
        reason: &'static str,
    },
    Initialize {
        lobby: String,
        players: Vec<Player>,
    },
    Join {
        player: Player,
    },
    Leave {
        player: <Player as gameplay::Player>::ID,
    },
    Ready {
        player: <Player as gameplay::Player>::ID,
        state: bool,
    },
    Start,
}

impl Message {
    const fn name(&self) -> &'static str {
        match self {
            Self::SelfLeave { .. } => unreachable!(),
            Self::Error { .. } => "error",
            Self::Initialize { .. } => "init",
            Self::Join { .. } => "join",
            Self::Leave { .. } => "leave",
            Self::Ready { .. } => "ready",
            Self::Start { .. } => "start",
        }
    }
}

impl Protected<Lobby<Player>> {
    #[allow(clippy::significant_drop_in_scrutinee)]
    async fn broadcast(&self, msg: &Message) {
        for player in self.lock().await.players().values() {
            player.sender.send(msg.clone()).unwrap();
        }
    }

    #[allow(clippy::significant_drop_in_scrutinee)]
    fn blocking_broadcast(&self, msg: &Message) {
        for player in self.blocking_lock().players().values() {
            player.sender.send(msg.clone()).unwrap();
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Protected<Lobby<Player>> {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let Some(lobby) = request.cookies().get_private("lobby") else {
            return Outcome::Error((Status::NotFound, ()));
        };
        let lobbys = request
            .guard::<&State<GlobalState>>()
            .await
            .unwrap()
            .lobbys
            .lock()
            .await;

        lobbys.get(lobby.value()).map_or_else(
            || Outcome::Error((Status::NotFound, ())),
            |x| Outcome::Success(Self::clone(x)),
        )
    }
}

struct ConnectionGuard<'a> {
    lobbys: &'a Mutex<HashMap<String, Protected<Lobby<Player>>>>,
    lobby: Protected<Lobby<Player>>,
    id: <Player as gameplay::Player>::ID,
}

impl<'a> Drop for ConnectionGuard<'a> {
    fn drop(&mut self) {
        self.lobby
            .blocking_broadcast(&Message::Leave { player: self.id });
        {
            let mut lobby = self.lobby.blocking_lock();
            lobby.remove_player(self.id);

            if lobby.players().is_empty() {
                self.lobbys.blocking_lock().remove(lobby.name());
            }
        }
    }
}

#[get("/lobby/create?<id>&<name>")]
#[must_use]
async fn create(id: Option<String>, name: String, state: &State<GlobalState>) -> Redirect {
    let mut id = id
        .unwrap_or_else(|| Alphanumeric.sample_string(&mut rand::thread_rng(), 6))
        .to_uppercase();

    {
        let mut lobbys = state.lobbys.lock().await;
        let games = state.games.lock().await;

        while lobbys.contains_key(&id) || games.contains_key(&id) {
            id = Alphanumeric
                .sample_string(&mut rand::thread_rng(), 6)
                .to_uppercase();
        }

        lobbys.insert(id.clone(), Protected::new(Lobby::new(id.clone())));
    }

    let id_copy = id.clone();
    let lobbys_ref = Arc::downgrade(&state.lobbys);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let lobbys = lobbys_ref.upgrade()?;
        {
            let mut lobbys = lobbys.lock().await;

            if lobbys.get(&id)?.lock().await.players().is_empty() {
                lobbys.remove(&id);
            }
        }

        Some(())
    });

    Redirect::to(uri!(join(id_copy, name)))
}

#[get("/lobby/join?<lobby>&<name>")]
#[must_use]
async fn join(
    lobby: &str,
    name: String,
    state: &State<GlobalState>,
    jar: &CookieJar<'_>,
) -> Redirect {
    let lobby_name = lobby.to_uppercase();

    let lobbys = state.lobbys.lock().await;
    let Some(lobby) = lobbys.get(&lobby_name).map(Protected::lock) else {
        return Redirect::to("/gameMenu.html?error=Lobby%20not%20found");
    };
    let lobby = lobby.await;

    let mut id = random();
    while lobby.players().contains_key(&id) {
        id = random();
    }
    drop(lobby);
    drop(lobbys);

    jar.add_private(("lobby", lobby_name));
    jar.add_private(("id", id.to_string()));
    jar.add_private(("name", name));

    Redirect::to(uri!("/lobby.html"))
}

// WARNING: EventStream is broken with rust 1.74.X, stay on 1.73.X until this is fixed
#[get("/lobby/events")]
#[must_use]
fn events<'a>(
    lobby: Option<Protected<Lobby<Player>>>,
    state: &'a State<GlobalState>,
    jar: &'a CookieJar<'_>,
    mut end: Shutdown,
) -> EventStream![Event + 'a] {
    EventStream! {
        let Some(lobby) = lobby else {
            yield make_event!(Message::Error {
                reason: "You are not in a lobby"
            });
            return;
        };

        let Some(Ok(id)) = jar.get_private("id").map(|x| x.value().parse::<<Player as gameplay::Player>::ID>()) else {
            yield make_event!(Message::Error {
                reason: "Invalid player id"
            });
            return;
        };

        let Some(name) = jar.get_private("name").map(|x| x.value().to_owned()) else {
            yield make_event!(Message::Error {
                reason: "Invalid player name"
            });
            return;
        };

        let (sender, mut receiver) = unbounded_channel();
        let player = Player { id, name, ready: false, sender };

        let result = lobby.lock().await.add_player(player.clone());
        match result {
            Ok(()) => (),
            Err(errors::Join::GameFull) => {
                yield make_event!(Message::Error {
                    reason: "This lobby is full"
                });
                return;
            }
            Err(errors::Join::AlreadyConnected) => {
                yield make_event!(Message::Error {
                    reason: "You are already connected to this game"
                });
                return;
            }
        }

        let lobby_name = lobby.lock().await.name().to_owned();
        yield make_event!(Message::Initialize {
            lobby: lobby_name,
            players: lobby.lock().await.players().values().cloned().collect(),
        });

        lobby.broadcast(&Message::Join { player }).await;

        let guard = ConnectionGuard { lobbys: &state.lobbys, lobby, id };

        loop {
            let Some(msg) = select! {
                msg = receiver.recv() => msg,
                () = &mut end => {
                    yield make_event!(Message::Error {
                        reason: "Server closed",
                    });
                    return;
                },
            } else { break; };
            if matches!(msg, Message::SelfLeave) {
                break;
            }

            yield make_event!(msg.clone());

            if matches!(msg, Message::Start { .. }) {
                break;
            }
        }

        drop(guard);
    }.heartbeat(Duration::from_secs(5))
}

#[get("/lobby/ready?<state>")]
#[allow(clippy::needless_pass_by_value)]
async fn ready(state: bool, lobby: Protected<Lobby<Player>>, jar: &CookieJar<'_>) {
    let Some(Ok(id)) = jar
        .get_private("id")
        .map(|x| x.value().parse::<<Player as gameplay::Player>::ID>())
    else {
        return;
    };

    if lobby.lock().await.get_player(id).is_some() {
        lobby.lock().await.get_player_mut(id).unwrap().ready = state;
        lobby.broadcast(&Message::Ready { player: id, state }).await;
    };
}

#[get("/lobby/leave")]
#[must_use]
async fn leave(lobby: Option<Protected<Lobby<Player>>>, jar: &CookieJar<'_>) -> Redirect {
    if let Some(Ok(id)) = jar
        .get_private("id")
        .map(|x| x.value().parse::<<Player as gameplay::Player>::ID>())
    {
        if let Some(lobby) = lobby {
            if let Some(player) = lobby.lock().await.get_player(id) {
                player.sender.send(Message::SelfLeave).unwrap();
            }
        }
    };

    jar.remove_private("lobby");
    jar.remove_private("id");
    jar.remove_private("name");

    Redirect::to("/gameMenu.html")
}

#[get("/lobby/start")]
#[allow(clippy::significant_drop_in_scrutinee, clippy::similar_names)]
async fn start(state: &State<GlobalState>, jar: &CookieJar<'_>) -> Status {
    let Some(lobby) = jar.get_private("lobby") else {
        return Status::NotFound;
    };

    let lobby = {
        let mut lobbys = state.lobbys.lock().await;
        let name = {
            let Some(lobby) = lobbys.get(lobby.value()) else {
                return Status::NotFound;
            };
            let locked = lobby.lock().await;
            if !locked.may_start() {
                return Status::PreconditionRequired;
            }

            locked.name().to_owned()
        };

        lobbys.remove(&name).unwrap()
    };

    let game: Game<game::Player> = lobby.lock().await.start();
    let name = game.name().to_owned();
    state.games.lock().await.insert(name, Protected::new(game));

    for player in lobby.lock().await.players().values() {
        player.sender.send(Message::Start).unwrap();
    }

    let games_ref = Arc::downgrade(&state.games);
    let id = lobby.lock().await.name().to_owned();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        let games = games_ref.upgrade()?;
        {
            let mut games = games.lock().await;

            if !games
                .get(&id)?
                .lock()
                .await
                .players()
                .values()
                .any(PlayingPlayer::connected)
            {
                games.remove(&id);
            }
        }

        Some(())
    });

    Status::Ok
}

pub fn routes() -> Vec<rocket::Route> {
    routes![create, join, events, ready, leave, start]
}
