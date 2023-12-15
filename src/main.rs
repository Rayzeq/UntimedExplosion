#![allow(clippy::option_if_let_else, clippy::no_effect_underscore_binding)]

use rand::{
    distributions::{Alphanumeric, DistString},
    random,
};
use rocket::{
    fs::{relative, FileServer},
    get,
    http::{CookieJar, Status},
    launch,
    request::{FromRequest, Outcome, Request},
    response::Redirect,
    routes, uri, State,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

mod game;

use game::{errors::PlayerJoinError, Game};

struct Player {
    id: usize,
    connected: bool,
    pub name: String,
}

impl game::Player for Player {
    fn id(&self) -> usize {
        self.id
    }

    fn connected(&self) -> bool {
        self.connected
    }

    fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }
}

struct ProtectedGame(Arc<Mutex<Game<Player>>>);

impl ProtectedGame {
    pub fn new(name: String) -> Self {
        Self(Arc::new(Mutex::new(Game::new(name))))
    }

    pub fn get(&self) -> MutexGuard<Game<Player>> {
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
fn test(game: ProtectedGame, jar: &CookieJar<'_>) -> String {
    let game = game.get();
    game.name().to_owned()
        + " "
        + &game
            .get_player(jar.get_private("userid").unwrap().value().parse().unwrap())
            .unwrap()
            .name
}

#[get("/create?<name>&<username>")]
#[must_use]
fn create(name: Option<String>, username: String, games: &State<Games>) -> Redirect {
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

    Redirect::to(uri!(join(name, username)))
}

#[get("/join?<lobby>&<name>")]
#[must_use]
fn join(lobby: String, name: String, games: &State<Games>, jar: &CookieJar<'_>) -> Redirect {
    if !games.games.lock().unwrap().contains_key(&lobby) {
        return Redirect::to("/lobby.html");
    }

    let id = random();
    let player = Player {
        id,
        connected: true,
        name,
    };

    let result = games.games.lock().unwrap()[&lobby].get().add_player(player);
    match result {
        Ok(()) => (),
        Err(PlayerJoinError::GameFull) => return Redirect::to("/lobby1.html"),
        Err(PlayerJoinError::GameAlreadyStarted) => return Redirect::to("/lobby2.html"),
    };

    jar.add_private(("lobby", lobby));
    jar.add_private(("userid", id.to_string()));

    Redirect::to(uri!("/ingame.html"))
}

#[get("/")]
fn index() -> Redirect {
    Redirect::to("/gameMenu.html")
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", FileServer::from(relative!("static")))
        .manage(Games::new())
        .mount("/", routes![index, create, join, test])
}
