use crate::{
    common::{make_event, GlobalState, Protected},
    gameplay::{self, errors, Cable, CutOutcome, Game, PlayingPlayer, Room, Team, WaitingPlayer},
};
use rand::{seq::SliceRandom, thread_rng};
use rocket::{
    get,
    http::{CookieJar, Status},
    request::{FromRequest, Outcome, Request},
    response::{
        status::BadRequest,
        stream::{Event, EventStream},
    },
    routes,
    serde::Serialize,
    tokio::{
        self, select,
        sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    },
    Shutdown, State,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

#[derive(Debug)]
pub struct Player {
    id: <Self as gameplay::Player>::ID,
    name: String,
    team: Team,
    cables: Vec<Cable>,
    revealed_cables: Vec<Cable>,
    sender: UnboundedSender<Message>,
    receiver: Option<Mutex<UnboundedReceiver<Message>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
struct PlayerData {
    id: <Player as gameplay::Player>::ID,
    name: String,
    revealed_cables: Vec<Cable>,
    connected: bool,
}

impl Player {
    fn clone_data(&self) -> PlayerData {
        PlayerData {
            id: self.id,
            name: self.name.clone(),
            revealed_cables: self.revealed_cables.clone(),
            connected: self.receiver.is_none(),
        }
    }
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

impl gameplay::PlayingPlayer for Player {
    fn new<T: WaitingPlayer<ID = Self::ID>>(player: &T, team: Team) -> Self {
        let (sender, receiver) = unbounded_channel();
        Self {
            id: player.id(),
            name: player.name().to_owned(),
            team,
            cables: Vec::new(),
            revealed_cables: Vec::new(),
            sender,
            receiver: Some(Mutex::new(receiver)),
        }
    }

    fn connected(&self) -> bool {
        self.receiver.is_none()
    }

    fn team(&self) -> Team {
        self.team
    }

    fn cables(&self) -> &[Cable] {
        &self.cables
    }

    fn set_cables(&mut self, cables: Vec<Cable>) {
        self.cables = cables;
    }

    fn cut_cable(&mut self) -> Cable {
        self.cables.shuffle(&mut thread_rng());
        let cutted = self.cables.pop().unwrap();
        self.revealed_cables.push(cutted);
        cutted
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
#[serde(untagged)]
enum Message {
    Error {
        reason: &'static str,
    },
    Initialize {
        lobby: String,
        players: Vec<PlayerData>,
        team: Team,
        wire_cutters: <Player as gameplay::Player>::ID,
    },
    Connect {
        player: <Player as gameplay::Player>::ID,
    },
    Disconnect {
        player: <Player as gameplay::Player>::ID,
    },
    RoundStart {
        cables: Vec<Cable>,
    },
    Cut {
        player: <Player as gameplay::Player>::ID,
        cable: Cable,
    },
    Win {
        team: Team,
        players: Vec<<Player as gameplay::Player>::ID>,
    },
}

impl Message {
    const fn name(&self) -> &'static str {
        match self {
            Self::Error { .. } => "error",
            Self::Initialize { .. } => "init",
            Self::Connect { .. } => "connect",
            Self::Disconnect { .. } => "disconnect",
            Self::RoundStart { .. } => "round_start",
            Self::Cut { .. } => "cut",
            Self::Win { .. } => "win",
        }
    }
}

impl Protected<Game<Player>> {
    #[allow(clippy::significant_drop_in_scrutinee)]
    fn broadcast(&self, msg: &Message) {
        for player in self.lock().players().values() {
            player.sender.send(msg.clone()).unwrap();
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Protected<Game<Player>> {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let Some(lobby) = request.cookies().get_private("lobby") else {
            return Outcome::Error((Status::NotFound, ()));
        };
        let games = request
            .guard::<&State<GlobalState>>()
            .await
            .unwrap()
            .games
            .lock()
            .unwrap();

        games
            .get(lobby.value())
            .map(Self::clone)
            .map_or_else(|| Outcome::Error((Status::NotFound, ())), Outcome::Success)
    }
}

struct ConnectionGuard {
    game: Protected<Game<Player>>,
    id: <Player as gameplay::Player>::ID,
    // we need the Option here because the destructor takes self by reference
    // which mean we need Option::take to save the receiver from being destroyed
    receiver: Option<UnboundedReceiver<Message>>,
    games: Option<Weak<Mutex<HashMap<String, Protected<Game<Player>>>>>>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.game
            .broadcast(&Message::Disconnect { player: self.id });

        let mut game = self.game.lock();

        game.get_player_mut(self.id)
            .unwrap()
            .receiver
            .replace(Mutex::new(self.receiver.take().unwrap()));
        let id = game.name().to_owned();
        let game_empty = !game.players().values().any(PlayingPlayer::connected);
        drop(game);

        if game_empty {
            let games = self.games.take().unwrap();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(60 * 5)).await;
                let games = games.upgrade()?;
                {
                    let mut games = games.lock().unwrap();

                    if !games
                        .get(&id)?
                        .lock()
                        .players()
                        .values()
                        .any(PlayingPlayer::connected)
                    {
                        games.remove(&id);
                    }
                }

                Some(())
            });
        }
    }
}

fn send_round(game: &Protected<Game<Player>>) {
    #[allow(clippy::significant_drop_in_scrutinee)]
    for player in game.lock().players().values() {
        player
            .sender
            .send(Message::RoundStart {
                cables: player.cables().to_owned(),
            })
            .unwrap();
    }
}

fn game_won(
    state: &State<GlobalState>,
    game: &Protected<Game<Player>>,
    team: Team,
    jar: &CookieJar<'_>,
) {
    let winning_players = game
        .lock()
        .players()
        .values()
        .filter(|p| p.team() == team)
        .map(gameplay::Player::id)
        .collect();
    game.broadcast(&Message::Win {
        team,
        players: winning_players,
    });

    let lobby = &game.lock().name().to_owned();
    state.games.lock().unwrap().remove(lobby);

    jar.remove_private("lobby");
    jar.remove_private("id");
    jar.remove_private("name");
}

// WARNING: EventStream is broken with rust 1.74.X, stay on 1.73.X until this is fixed
#[get("/game/events")]
#[must_use]
fn events<'a>(
    game: Option<Protected<Game<Player>>>,
    state: &'a State<GlobalState>,
    jar: &'a CookieJar<'_>,
    mut end: Shutdown,
) -> EventStream![Event + 'a] {
    EventStream! {
        let Some(game) = game else {
            yield make_event!(Message::Error {
                reason: "You are not in a game"
            });
            return;
        };

        let Some(Ok(id)) = jar.get_private("id").map(|x| x.value().parse::<<Player as gameplay::Player>::ID>()) else {
            yield make_event!(Message::Error {
                reason: "Invalid player id"
            });
            return;
        };

        if game.lock().get_player(id).is_none() {
            yield make_event!(Message::Error {
                    reason: "You are not part of this game",
                });
            return;
        };

        let Some(receiver) = game.lock().get_player_mut(id).unwrap().receiver.take() else {
            yield make_event!(Message::Error {
                    reason: "You are already connected to this game",
                });
            return;
        };
        let mut receiver = receiver.into_inner().unwrap();
        // discard all previous messages
        while receiver.try_recv().is_ok() {}

        let msg = {
            let game = game.lock();
            let lobby_name = game.name().to_owned();
            let player_list = game.players().values().map(Player::clone_data).collect();
            let team = game.get_player(id).unwrap().team();
            let wire_cutters = game.wire_cutters;
            drop(game);
            Message::Initialize { lobby: lobby_name, players: player_list, team, wire_cutters }
        };
        yield make_event!(msg);
        yield make_event!(&Message::RoundStart {
            cables: game.lock().get_player(id).unwrap().cables().to_owned()
        });

        game.broadcast(&Message::Connect { player: id });

        let mut guard = ConnectionGuard {
            game,
            id,
            receiver: Some(receiver),
            games: Some(Arc::downgrade(&state.games)),
        };

        let receiver = guard.receiver.as_mut().unwrap();

        loop {
            let Some(msg) = select! {
                msg = receiver.recv() => msg,
                () = &mut end => {
                    yield make_event!(Message::Error {
                        reason: "Server closed",
                    });
                    break
                },
            } else { break; };

            yield make_event!(msg.clone());

            if matches!(msg, Message::Win { .. }) {
                break;
            }
        }
    }.heartbeat(Duration::from_secs(5))
}

#[get("/game/cut?<player>")]
#[allow(clippy::needless_pass_by_value)]
fn cut(
    player: <Player as gameplay::Player>::ID,
    game: Protected<Game<Player>>,
    state: &State<GlobalState>,
    jar: &CookieJar<'_>,
) -> Result<(), BadRequest<&'static str>> {
    let Some(Ok(id)) = jar
        .get_private("id")
        .map(|x| x.value().parse::<<Player as gameplay::Player>::ID>())
    else {
        return Err(BadRequest("Invalid player id"));
    };

    if game.lock().get_player(id).is_none() {
        return Err(BadRequest("You are not part of this game"));
    };

    if game.lock().get_player(player).is_none() {
        return Err(BadRequest(
            "The player you specified is not part of this game",
        ));
    };

    let result = game.lock().cut(id, player);
    let (cable, outcome) = match result {
        Ok(x) => x,
        Err(errors::Cut::DontHaveWireCutter) => {
            return Err(BadRequest("You don't have the wire cutter"))
        }
        Err(errors::Cut::CannotSelfCut) => {
            return Err(BadRequest("You can't cut one of your own cables"))
        }
    };

    game.broadcast(&Message::Cut { player, cable });

    match outcome {
        CutOutcome::Nothing => (),
        CutOutcome::Win(team) => game_won(state, &game, team, jar),
        CutOutcome::RoundEnd => {
            if game.lock().next_round() {
                game_won(state, &game, Team::Moriarty, jar);
            } else {
                send_round(&game);
            }
        }
    }

    Ok(())
}

pub fn routes() -> Vec<rocket::Route> {
    routes![events, cut]
}
