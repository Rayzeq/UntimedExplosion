pub use crate::PlayerWrapper;
use crate::{
    game::{Game, Lobby, Player, Room},
    lobby,
};
use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex, MutexGuard},
};

macro_rules! make_event {
    ($message:expr) => {{
        let __msg = $message;
        Event::json(&__msg).event(__msg.name())
    }};
}

pub(crate) use make_event;

pub struct GlobalState {
    pub lobbys: Mutex<HashMap<String, Protected<lobby::Player, Lobby<lobby::Player>>>>,
    pub games: Mutex<HashMap<String, Protected<crate::PlayerWrapper, Game<crate::PlayerWrapper>>>>,
}

impl GlobalState {
    pub fn new() -> Self {
        Self {
            lobbys: Mutex::new(HashMap::new()),
            games: Mutex::new(HashMap::new()),
        }
    }
}

pub struct Protected<PLAYER: Player, ROOM: Room<PLAYER>>(Arc<Mutex<ROOM>>, PhantomData<PLAYER>);

impl<PLAYER: Player, ROOM: Room<PLAYER>> Protected<PLAYER, ROOM> {
    pub fn new(content: ROOM) -> Self {
        Self(Arc::new(Mutex::new(content)), PhantomData)
    }

    pub fn lock(&self) -> MutexGuard<ROOM> {
        self.0.lock().unwrap()
    }
}

impl<PLAYER: Player, ROOM: Room<PLAYER>> Clone for Protected<PLAYER, ROOM> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0), PhantomData)
    }
}
