use std::collections::HashMap;

use errors::PlayerJoinError;

pub trait Player {
    fn id(&self) -> usize;
    fn connected(&self) -> bool;
    fn set_connected(&mut self, connected: bool);
}

enum GameState {
    Lobby { ready_players: Vec<usize> },
    Ingame,
}

pub struct Game<PLAYER: Player> {
    name: String,
    players: HashMap<usize, PLAYER>,
    state: GameState,
}

impl<PLAYER: Player> Game<PLAYER> {
    pub fn new(name: String) -> Self {
        Self {
            name,
            players: HashMap::new(),
            state: GameState::Lobby {
                ready_players: Vec::new(),
            },
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn add_player(&mut self, player: PLAYER) -> Result<(), PlayerJoinError> {
        match self.state {
            GameState::Lobby { .. } => {
                if self.players.len() >= 8 {
                    return Err(PlayerJoinError::GameFull);
                }

                self.players
                    .entry(player.id())
                    .or_insert(player)
                    .set_connected(true);

                Ok(())
            }
            _ => Err(PlayerJoinError::GameAlreadyStarted),
        }
    }

    pub fn get_player(&self, id: usize) -> Option<&PLAYER> {
        self.players.get(&id)
    }
}

pub mod errors {
    pub enum PlayerJoinError {
        GameFull,
        GameAlreadyStarted,
    }
}
