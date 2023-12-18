#![allow(clippy::option_if_let_else, clippy::no_effect_underscore_binding)]

use rand::{seq::SliceRandom, thread_rng};
use rocket::{
    fs::{relative, FileServer},
    get,
    http::{CookieJar, Status},
    launch,
    request::{FromRequest, Outcome, Request},
    response::{
        status::BadRequest,
        stream::{Event, EventStream},
        Redirect,
    },
    routes,
    serde::Serialize,
    tokio::{
        select,
        sync::mpsc::{UnboundedReceiver, UnboundedSender},
    },
    Shutdown, State,
};
use std::sync::Mutex;

mod common;
mod game;
mod lobby;

use common::{GlobalState, Protected};
use game::{old_errors, Cable, CutOutcome, Game, OldPlayer as _, Room, Team};

// TODO: auto-delete created game if nobody joins
// TODO: delete game when it goes empty

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
#[serde(untagged)]
pub enum Message {
    Error {
        reason: String,
    },
    Initialize {
        lobby: String,
        players: Vec<Player>,
        team: Option<Team>,
        wire_cutters: Option<<PlayerWrapper as game::Player>::ID>,
    },
    Join {
        player: Player,
    },
    Leave {
        player: <PlayerWrapper as game::Player>::ID,
    },
    SelfLeave,
    Ready {
        player: <PlayerWrapper as game::Player>::ID,
        state: bool,
    },
    Start {
        team: Team,
    },
    RoundStart {
        cables: Vec<Cable>,
        wire_cutters: <PlayerWrapper as game::Player>::ID,
    },
    Cut {
        player: <PlayerWrapper as game::Player>::ID,
        cable: Cable,
    },
    Win {
        team: Team,
        players: Vec<<PlayerWrapper as game::Player>::ID>,
    },
}

impl Message {
    const fn name(&self) -> &'static str {
        match self {
            Self::Error { .. } => "error",
            Self::Initialize { .. } => "init",
            Self::Join { .. } => "join",
            Self::Leave { .. } => "leave",
            Self::SelfLeave { .. } => unreachable!(),
            Self::Ready { .. } => "ready",
            Self::Start { .. } => "start",
            Self::RoundStart { .. } => "round_start",
            Self::Cut { .. } => "cut",
            Self::Win { .. } => "win",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
pub struct Player {
    id: <PlayerWrapper as game::Player>::ID,
    pub name: String,
    pub ready: bool,
    #[serde(skip)]
    pub cables: Vec<Cable>,
    pub revealed_cables: Vec<Cable>,
}

impl Player {
    pub fn new(id: <PlayerWrapper as game::Player>::ID, name: String) -> Self {
        Self {
            id,
            name,
            ready: false,
            cables: Vec::new(),
            revealed_cables: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct PlayerWrapper {
    pub inner: Player,
    pub sender: UnboundedSender<Message>,
    // only present if no SSE stream is currently using it
    pub receiver: Option<Mutex<UnboundedReceiver<Message>>>,
    team: Option<game::Team>,
}

impl game::Player for PlayerWrapper {
    type ID = u32;

    fn id(&self) -> Self::ID {
        self.inner.id
    }
}

impl game::WaitingPlayer for PlayerWrapper {
    fn ready(&self) -> bool {
        self.inner.ready
    }
}

impl game::OldPlayer for PlayerWrapper {
    fn connected(&self) -> bool {
        self.receiver.is_none()
    }

    fn set_team(&mut self, team: game::Team) {
        self.team = Some(team);
    }

    fn team(&self) -> game::Team {
        self.team.unwrap()
    }

    fn set_cables(&mut self, cables: Vec<game::Cable>) {
        self.inner.cables = cables;
    }

    fn cut_cable(&mut self) -> game::Cable {
        self.inner.cables.shuffle(&mut thread_rng());
        let cutted = self.inner.cables.pop().unwrap();
        self.inner.revealed_cables.push(cutted);
        cutted
    }

    fn cables(&self) -> &[game::Cable] {
        &self.inner.cables
    }
}

impl Protected<PlayerWrapper, Game<PlayerWrapper>> {
    pub fn broadcast(&self, msg: &Message) {
        for player in self.lock().players().values() {
            player.sender.send(msg.clone()).unwrap();
        }
    }
}

impl Protected<PlayerWrapper, Game<PlayerWrapper>> {
    pub fn new_game(name: String) -> Self {
        Self::new(Game::new(name))
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Protected<PlayerWrapper, Game<PlayerWrapper>> {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        if let Some(lobby) = request.cookies().get_private("lobby") {
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
        } else {
            Outcome::Error((Status::NotFound, ()))
        }
    }
}

fn get_playerid(jar: &CookieJar<'_>) -> Result<<PlayerWrapper as game::Player>::ID, Status> {
    if let Some(id) = jar.get_private("userid") {
        id.value().parse().map_err(|_| Status::BadRequest)
    } else {
        Err(Status::NotFound)
    }
}

struct ConnectionGuard {
    game: Protected<PlayerWrapper, Game<PlayerWrapper>>,
    playerid: <PlayerWrapper as game::Player>::ID,
    receiver: Option<UnboundedReceiver<Message>>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.game.broadcast(&Message::Leave {
            player: self.playerid,
        });
        self.game
            .lock()
            .get_player_mut(self.playerid)
            .unwrap()
            .receiver
            .replace(Mutex::new(self.receiver.take().unwrap()));

        if !self.game.lock().started() {
            self.game.lock().remove_player(self.playerid);
        }
    }
}

// WARNING: this DOES NOT work with rust 1.74.X, stay on 1.73.X until this is fixed
#[get("/events")]
#[must_use]
fn events<'a>(
    game: Protected<PlayerWrapper, Game<PlayerWrapper>>,
    jar: &'a CookieJar<'a>,
    mut end: Shutdown,
) -> EventStream![Event + 'a] {
    macro_rules! make_event {
        ($message:expr) => {{
            let __msg = $message;
            Event::json(&__msg).event(__msg.name())
        }};
    }

    EventStream! {
        let playerid = match get_playerid(jar) {
            Ok(playerid) => playerid,
            Err(e) => {
                yield make_event!(Message::Error {
                    reason: e.to_string(),
                });
                return;
            }
        };

        if game.lock().get_player(playerid).is_none() {
            yield make_event!(Message::Error {
                    reason: "You are not part of this game".to_owned(),
                });
            return;
        };
        let player = game.lock().get_player(playerid).unwrap().inner.clone();
        let Some(receiver) = game.lock().get_player_mut(playerid).unwrap().receiver.take() else {
            yield make_event!(Message::Error {
                    reason: "You are already connected to this game".to_owned(),
                });
            return;
        };
        let receiver = receiver.into_inner().unwrap();
        game.broadcast(&Message::Join { player: player.clone() });

        let lobby_name = game.lock().name().to_owned();
        let player_list = game.lock().players().values().map(|p| p.inner.clone()).collect();
        let team = game.lock().get_player(playerid).unwrap().team;
        let wire_cutters = game.lock().wire_cutters();
        yield make_event!(Message::Initialize { lobby: lobby_name, players: player_list, team, wire_cutters });

        let mut guard = ConnectionGuard {
            game,
            playerid,
            receiver: Some(receiver),
        };

        let receiver = guard.receiver.as_mut().unwrap();

        loop {
            let msg = select! {
                msg = receiver.recv() => msg,
                _ = &mut end => {
                    yield make_event!(Message::Error {
                        reason: "Server closed".to_owned(),
                    });
                    break
                },
            };
            let Some(msg) = msg else {
                break;
            };
            if matches!(msg, Message::SelfLeave) {
                break;
            }

            yield make_event!(msg.clone());

            if matches!(msg, Message::Win { .. }) {
                break;
            }
        }
    }
}

#[get("/leave")]
#[must_use]
fn leave(game: Protected<PlayerWrapper, Game<PlayerWrapper>>, jar: &CookieJar<'_>) -> Redirect {
    let Ok(playerid) = get_playerid(jar) else {
        return Redirect::to("/gameMenu.html");
    };

    if let Some(player) = game.lock().get_player(playerid) {
        player.sender.send(Message::SelfLeave).unwrap();
    }

    jar.remove_private("lobby");
    jar.remove_private("userid");

    Redirect::to("/gameMenu.html")
}

#[get("/ready?<state>")]
fn ready(state: bool, game: Protected<PlayerWrapper, Game<PlayerWrapper>>, jar: &CookieJar<'_>) {
    let Ok(playerid) = get_playerid(jar) else {
        return;
    };

    if game.lock().get_player(playerid).is_some() {
        game.lock().get_player_mut(playerid).unwrap().inner.ready = state;
        game.broadcast(&Message::Ready {
            player: playerid,
            state,
        });
    };
}

#[get("/start")]
fn start(game: Protected<PlayerWrapper, Game<PlayerWrapper>>) -> Status {
    if game.lock().start().is_err() {
        return Status::PreconditionRequired;
    }

    for player in game.lock().players().values() {
        player
            .sender
            .send(Message::Start {
                team: player.team(),
            })
            .unwrap();
    }
    send_round(&game);

    Status::Ok
}

#[get("/cut?<player>")]
fn cut(
    player: <PlayerWrapper as game::Player>::ID,
    game: Protected<PlayerWrapper, Game<PlayerWrapper>>,
    games: &State<GlobalState>,
    jar: &CookieJar<'_>,
) -> Result<(), BadRequest<&'static str>> {
    let Ok(playerid) = get_playerid(jar) else {
        return Err(BadRequest("Invalid player id"));
    };

    if game.lock().get_player(playerid).is_none() {
        return Err(BadRequest("You are not part of this game"));
    };

    if game.lock().get_player(player).is_none() {
        return Err(BadRequest(
            "The player you specified is not part of this game",
        ));
    };

    let (cable, outcome) = match game.lock().cut(playerid, player) {
        Ok(x) => x,
        Err(old_errors::Cut::GameNotStarted) => {
            return Err(BadRequest("This game hasn't started yet"))
        }
        Err(old_errors::Cut::DontHaveWireCutter) => {
            return Err(BadRequest("You don't have the wire cutter"))
        }
        Err(old_errors::Cut::CannotSelfCut) => {
            return Err(BadRequest("You can't cut one of your own cables"))
        }
    };

    game.broadcast(&Message::Cut { player, cable });

    match outcome {
        CutOutcome::Nothing => (),
        CutOutcome::Win(team) => game_won(games, &game, team, jar),
        CutOutcome::RoundEnd => {
            if game.lock().next_round() {
                game_won(games, &game, Team::Moriarty, jar);
            } else {
                send_round(&game);
            }
        }
    }

    Ok(())
}

fn send_round(game: &Protected<PlayerWrapper, Game<PlayerWrapper>>) {
    let wire_cutters = game.lock().wire_cutters();
    for player in game.lock().players().values() {
        player
            .sender
            .send(Message::RoundStart {
                cables: player.cables().to_owned(),
                wire_cutters: wire_cutters.unwrap(),
            })
            .unwrap();
    }
}

fn game_won(
    games: &State<GlobalState>,
    game: &Protected<PlayerWrapper, Game<PlayerWrapper>>,
    team: Team,
    jar: &CookieJar<'_>,
) {
    let winning_players = game
        .lock()
        .players()
        .values()
        .filter(|p| p.team() == team)
        .map(game::Player::id)
        .collect();
    game.broadcast(&Message::Win {
        team,
        players: winning_players,
    });

    let lobby = &game.lock().name().to_owned();
    games.games.lock().unwrap().remove(lobby);

    jar.remove_private("lobby");
    jar.remove_private("userid");
}

#[get("/")]
fn index() -> Redirect {
    Redirect::to("/gameMenu.html")
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .manage(GlobalState::new())
        .mount("/", FileServer::from(relative!("static")))
        .mount("/", routes![index, events, leave, ready, start, cut])
        .mount("/", lobby::routes())
}
