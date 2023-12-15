#![allow(clippy::option_if_let_else, clippy::no_effect_underscore_binding)]

use rand::distributions::{Alphanumeric, DistString};
use rocket::{
    fs::{relative, FileServer},
    get,
    http::{CookieJar, Status},
    launch,
    request::{FromRequest, Outcome, Request},
    response::Redirect,
    routes, State,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

enum Player {
    InLobby { name: String, ready: bool },
    InGame { name: String },
}

impl Player {
    fn name(&self) -> &str {
        match self {
            Self::InLobby { name, .. } | Self::InGame { name } => name,
        }
    }
}

enum Game {
    Lobby {
        name: String,
        players: HashMap<String, Player>,
    },
    Ingame {
        name: String,
        player: Vec<String>,
    },
}

impl Game {
    pub fn new(name: String) -> Self {
        Self::Lobby {
            name,
            players: HashMap::new(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Lobby { name, .. } | Self::Ingame { name, .. } => name,
        }
    }
}

struct ProtectedGame(Arc<Mutex<Game>>);

impl ProtectedGame {
    pub fn new(name: String) -> Self {
        Self(Arc::new(Mutex::new(Game::new(name))))
    }

    pub fn get(&self) -> MutexGuard<Game> {
        self.0.lock().unwrap()
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

#[get("/test")]
#[must_use]
fn test(game: ProtectedGame) -> String {
    game.get().name().to_owned()
}

#[get("/create?<name>")]
#[must_use]
fn create(name: Option<String>, games: &State<Games>, jar: &CookieJar<'_>) -> String {
    let mut name = name
        .unwrap_or_else(|| Alphanumeric.sample_string(&mut rand::thread_rng(), 8))
        .to_uppercase();

    {
        let mut games = games.games.lock().unwrap();

        while games.contains_key(&name) {
            name = Alphanumeric.sample_string(&mut rand::thread_rng(), 8);
        }

        games.insert(name.clone(), ProtectedGame::new(name.clone()));
    }

    jar.add_private(("lobby", name.clone()));
    name
}

#[get("/")]
fn index() -> Redirect {
    Redirect::to("/index.html")
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", FileServer::from(relative!("static")))
        .manage(Games::new())
        .mount("/", routes![index, create, test])
}
