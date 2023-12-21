use crate::{
    game,
    gameplay::{Game, Lobby},
    lobby,
};
use rocket::tokio::sync::{Mutex, MutexGuard};
use std::{collections::HashMap, sync::Arc};

macro_rules! make_event {
    ($message:expr) => {{
        let __msg = $message;
        Event::json(&__msg).event(__msg.name())
    }};
}

pub(crate) use make_event;

pub struct GlobalState {
    pub lobbys: Arc<Mutex<HashMap<String, Protected<Lobby<lobby::Player>>>>>,
    pub games: Arc<Mutex<HashMap<String, Protected<Game<game::Player>>>>>,
}

impl GlobalState {
    pub fn new() -> Self {
        Self {
            lobbys: Arc::new(Mutex::new(HashMap::new())),
            games: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

pub struct Protected<T>(Arc<Mutex<T>>);

impl<T: Send> Protected<T> {
    pub fn new(content: T) -> Self {
        Self(Arc::new(Mutex::new(content)))
    }

    pub async fn lock(&self) -> MutexGuard<T> {
        self.0.lock().await
    }

    pub fn blocking_lock(&self) -> MutexGuard<T> {
        self.0.blocking_lock()
    }
}

impl<T> Clone for Protected<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
