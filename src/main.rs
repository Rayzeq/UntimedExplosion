#![allow(clippy::option_if_let_else, clippy::no_effect_underscore_binding)]

use rand::{
    distributions::{Alphanumeric, DistString},
    random,
    seq::SliceRandom,
    thread_rng,
};
use rocket::{
    fs::{relative, FileServer},
    get,
    http::{CookieJar, Status},
    launch,
    request::{FromRequest, Outcome, Request},
    response::{
        stream::{Event, EventStream},
        Redirect,
    },
    routes,
    serde::{json::Json, Serialize},
    tokio::{
        select,
        sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    },
    uri, Responder, Shutdown, State,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

mod game;

use game::{
    errors::{self, PlayerJoin},
    Cable, CutOutcome, Game, Player as _, Team,
};

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "rocket::serde")]
#[serde(untagged)]
enum Message {
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
struct Player {
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
struct PlayerWrapper {
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

    fn ready(&self) -> bool {
        self.inner.ready
    }

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

struct ProtectedGame(Arc<Mutex<Game<PlayerWrapper>>>);

impl ProtectedGame {
    pub fn new(name: String) -> Self {
        Self(Arc::new(Mutex::new(Game::new(name))))
    }

    pub fn get(&self) -> MutexGuard<Game<PlayerWrapper>> {
        self.0.lock().unwrap()
    }

    pub fn broadcast(&self, msg: &Message) {
        for player in self.get().players().values() {
            player.sender.send(msg.clone()).unwrap();
        }
    }
}

impl Clone for ProtectedGame {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ProtectedGame {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        if let Some(lobby) = request.cookies().get_private("lobby") {
            let games = request
                .guard::<&State<Games>>()
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

struct Games {
    pub games: Mutex<HashMap<String, ProtectedGame>>,
}

impl Games {
    pub fn new() -> Self {
        Self {
            games: Mutex::new(HashMap::new()),
        }
    }
}

#[get("/create?<name>&<username>")]
#[must_use]
fn create(name: Option<String>, username: String, games: &State<Games>) -> Redirect {
    let mut name = name.unwrap_or_else(|| Alphanumeric.sample_string(&mut rand::thread_rng(), 6));

    {
        let mut games = games.games.lock().unwrap();

        while games.contains_key(&name) {
            name = Alphanumeric.sample_string(&mut rand::thread_rng(), 6);
        }
        name = name.to_uppercase();

        games.insert(name.clone(), ProtectedGame::new(name.clone()));
    }

    Redirect::to(uri!(join(name, username)))
}

#[get("/join?<lobby>&<name>")]
#[must_use]
fn join(lobby: String, name: String, games: &State<Games>, jar: &CookieJar<'_>) -> Redirect {
    let lobby = lobby.to_uppercase();

    if !games.games.lock().unwrap().contains_key(&lobby) {
        return Redirect::to("/gameMenu0.html");
    }

    let mut id = random();
    while games.games.lock().unwrap()[&lobby]
        .get()
        .players()
        .contains_key(&id)
    {
        id = random();
    }

    let (sender, receiver) = unbounded_channel();
    let player = PlayerWrapper {
        inner: Player::new(id, name),
        sender,
        receiver: Some(Mutex::new(receiver)),
        team: None,
    };

    let result = games.games.lock().unwrap()[&lobby].get().add_player(player);
    match result {
        Ok(()) => (),
        Err(PlayerJoin::GameFull) => return Redirect::to("/gameMenu1.html"),
        Err(PlayerJoin::GameAlreadyStarted) => return Redirect::to("/gameMenu2.html"),
    };

    jar.add_private(("lobby", lobby));
    jar.add_private(("userid", id.to_string()));

    Redirect::to(uri!("/events"))
}

fn get_playerid(jar: &CookieJar<'_>) -> Result<<PlayerWrapper as game::Player>::ID, Status> {
    if let Some(id) = jar.get_private("userid") {
        id.value().parse().map_err(|_| Status::BadRequest)
    } else {
        Err(Status::NotFound)
    }
}

struct ConnectionGuard {
    game: ProtectedGame,
    playerid: <PlayerWrapper as game::Player>::ID,
    receiver: Option<UnboundedReceiver<Message>>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.game.broadcast(&Message::Leave {
            player: self.playerid,
        });
        self.game
            .get()
            .get_player_mut(self.playerid)
            .unwrap()
            .receiver
            .replace(Mutex::new(self.receiver.take().unwrap()));

        if !self.game.get().started() {
            self.game.get().remove_player(self.playerid);
        }
    }
}

// WARNING: this DOES NOT work with rust 1.74.X, stay on 1.73.X until this is fixed
#[get("/events")]
#[must_use]
fn events<'a>(
    game: ProtectedGame,
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

        if game.get().get_player(playerid).is_none() {
            yield make_event!(Message::Error {
                    reason: "You are not part of this game".to_owned(),
                });
            return;
        };
        let player = game.get().get_player(playerid).unwrap().inner.clone();
        let Some(receiver) = game.get().get_player_mut(playerid).unwrap().receiver.take() else {
            yield make_event!(Message::Error {
                    reason: "You are already connected to this game".to_owned(),
                });
            return;
        };
        let receiver = receiver.into_inner().unwrap();
        game.broadcast(&Message::Join { player: player.clone() });

        let lobby_name = game.get().name().to_owned();
        let player_list = game.get().players().values().map(|p| p.inner.clone()).collect();
        let team = game.get().get_player(playerid).unwrap().team;
        let wire_cutters = game.get().wire_cutters();
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
                _ = &mut end => break,
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
fn leave(game: ProtectedGame, jar: &CookieJar<'_>) -> Redirect {
    let Ok(playerid) = get_playerid(jar) else {
        return Redirect::to("/gameMenu.html");
    };

    if let Some(player) = game.get().get_player(playerid) {
        player.sender.send(Message::SelfLeave).unwrap();
    }

    jar.remove_private("lobby");
    jar.remove_private("userid");

    Redirect::to("/gameMenu.html")
}

#[get("/ready?<state>")]
fn ready(state: bool, game: ProtectedGame, jar: &CookieJar<'_>) {
    let Ok(playerid) = get_playerid(jar) else {
        return;
    };

    if game.get().get_player(playerid).is_some() {
        game.get().get_player_mut(playerid).unwrap().inner.ready = state;
        game.broadcast(&Message::Ready {
            player: playerid,
            state,
        });
    };
}

#[get("/start")]
fn start(game: ProtectedGame) -> Status {
    if game.get().start().is_err() {
        return Status::PreconditionRequired;
    }

    for player in game.get().players().values() {
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
    game: ProtectedGame,
    games: &State<Games>,
    jar: &CookieJar<'_>,
) -> Result<(), &'static str> {
    let Ok(playerid) = get_playerid(jar) else {
        return Err("Invalid player id");
    };

    if game.get().get_player(playerid).is_none() {
        return Err("You are not part of this game");
    };

    if game.get().get_player(player).is_none() {
        return Err("The player you specified is not part of this game");
    };

    let (cable, outcome) = match game.get().cut(playerid, player) {
        Ok(x) => x,
        Err(errors::Cut::GameNotStarted) => return Err("This game hasn't started yet"),
        Err(errors::Cut::DontHaveWireCutter) => return Err("You don't have the wire cutter"),
        Err(errors::Cut::CannotSelfCut) => return Err("You can't cut one of your own cables"),
    };

    game.broadcast(&Message::Cut { player, cable });

    match outcome {
        CutOutcome::Nothing => (),
        CutOutcome::Win(team) => game_won(games, &game, team),
        CutOutcome::RoundEnd => {
            if game.get().next_round() {
                game_won(games, &game, Team::Moriarty);
            } else {
                send_round(&game);
            }
        }
    }

    Ok(())
}

fn send_round(game: &ProtectedGame) {
    let wire_cutters = game.get().wire_cutters();
    for player in game.get().players().values() {
        player
            .sender
            .send(Message::RoundStart {
                cables: player.cables().to_owned(),
                wire_cutters: wire_cutters.unwrap(),
            })
            .unwrap();
    }
}

fn game_won(games: &State<Games>, game: &ProtectedGame, team: Team) {
    let winning_players = game
        .get()
        .players()
        .values()
        .filter(|p| p.team() == team)
        .map(game::Player::id)
        .collect();
    game.broadcast(&Message::Win {
        team,
        players: winning_players,
    });

    let lobby = &game.get().name().to_owned();
    games.games.lock().unwrap().remove(lobby);
}

#[get("/")]
fn index() -> Redirect {
    Redirect::to("/gameMenu.html")
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .manage(Games::new())
        .mount("/", FileServer::from(relative!("static")))
        .mount(
            "/",
            routes![index, create, join, events, leave, ready, start, cut],
        )
}
