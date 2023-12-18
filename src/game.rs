use rand::{
    seq::{IteratorRandom, SliceRandom},
    thread_rng,
};
use rocket::serde::{Deserialize, Serialize};
use std::{collections::HashMap, hash::Hash};

macro_rules! repeated_vec {
    ($($quantity:expr => $value:expr),*) => {{
        let mut v = Vec::with_capacity(repeated_vec!(@sum $($quantity),*));
        $(
            v.extend(std::iter::repeat($value).take($quantity));
        )*
        v
    }};
    (@sum $quantity:expr) => {
        $quantity
    };
    (@sum $quantity:expr, $($quantities:expr),*) => {
        $quantity + repeated_vec!(@sum $($quantities),*)
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "rocket::serde")]
#[serde(rename_all = "lowercase")]
pub enum Team {
    Sherlock,
    Moriarty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "rocket::serde")]
#[serde(rename_all = "lowercase")]
pub enum Cable {
    Safe,
    Defusing,
    Bomb,
}

pub trait Player {
    type ID: Eq + Hash + Clone + Copy;

    fn id(&self) -> Self::ID;
}

pub trait Room<PLAYER: Player> {
    fn name(&self) -> &str;
    fn players(&self) -> &HashMap<PLAYER::ID, PLAYER>;
    fn get_player(&self, id: PLAYER::ID) -> Option<&PLAYER>;
    fn get_player_mut(&mut self, id: PLAYER::ID) -> Option<&mut PLAYER>;
}

pub trait WaitingPlayer: Player {
    fn ready(&self) -> bool;
}

pub struct Lobby<PLAYER: WaitingPlayer> {
    name: String,
    players: HashMap<PLAYER::ID, PLAYER>,
}

impl<PLAYER: WaitingPlayer> Lobby<PLAYER> {
    pub fn new(name: String) -> Self {
        Self {
            name,
            players: HashMap::new(),
        }
    }

    pub fn add_player(&mut self, player: PLAYER) -> Result<(), errors::Join> {
        if self.players.len() >= 8 {
            return Err(errors::Join::GameFull);
        }

        if self.players.contains_key(&player.id()) {
            return Err(errors::Join::AlreadyConnected);
        }
        self.players.insert(player.id(), player);

        Ok(())
    }

    pub fn remove_player(&mut self, id: PLAYER::ID) {
        self.players.remove(&id);
    }
}

impl<PLAYER: WaitingPlayer> Room<PLAYER> for Lobby<PLAYER> {
    fn name(&self) -> &str {
        &self.name
    }

    fn players(&self) -> &HashMap<PLAYER::ID, PLAYER> {
        &self.players
    }

    fn get_player(&self, id: PLAYER::ID) -> Option<&PLAYER> {
        self.players.get(&id)
    }

    fn get_player_mut(&mut self, id: PLAYER::ID) -> Option<&mut PLAYER> {
        self.players.get_mut(&id)
    }
}

pub mod errors {
    use thiserror::Error;

    #[derive(Error, Debug, Clone, Copy)]
    pub enum Join {
        #[error("this game is already full")]
        GameFull,
        #[error("you are already connected to this game")]
        AlreadyConnected,
    }
}

//
//
//
//
//
//
//
//
//
//
//
//
//

pub trait OldPlayer: WaitingPlayer {
    fn connected(&self) -> bool;

    fn team(&self) -> Team;
    fn set_team(&mut self, team: Team);

    fn set_cables(&mut self, cables: Vec<Cable>);
    fn cut_cable(&mut self) -> Cable;
    fn cables(&self) -> &[Cable];
}

pub enum CutOutcome {
    Win(Team),
    RoundEnd,
    Nothing,
}

enum GameState<PLAYER: OldPlayer> {
    Lobby,
    Ingame {
        wire_cutters: PLAYER::ID,
        defusing_remaining: usize,
        cutted_count: usize,
    },
}

pub struct Game<PLAYER: OldPlayer> {
    name: String,
    players: HashMap<PLAYER::ID, PLAYER>,
    state: GameState<PLAYER>,
}

impl<PLAYER: Player + OldPlayer> Room<PLAYER> for Game<PLAYER> {
    fn name(&self) -> &str {
        &self.name
    }

    fn players(&self) -> &HashMap<PLAYER::ID, PLAYER> {
        &self.players
    }

    fn get_player(&self, id: PLAYER::ID) -> Option<&PLAYER> {
        self.players.get(&id)
    }

    fn get_player_mut(&mut self, id: PLAYER::ID) -> Option<&mut PLAYER> {
        self.players.get_mut(&id)
    }
}

impl<PLAYER: OldPlayer> Game<PLAYER> {
    pub fn new(name: String) -> Self {
        Self {
            name,
            players: HashMap::new(),
            state: GameState::Lobby,
        }
    }

    pub const fn wire_cutters(&self) -> Option<PLAYER::ID> {
        if let GameState::Ingame { wire_cutters, .. } = self.state {
            Some(wire_cutters)
        } else {
            None
        }
    }

    pub fn remove_player(&mut self, id: PLAYER::ID) {
        self.players.remove(&id);
    }

    pub const fn started(&self) -> bool {
        matches!(self.state, GameState::Ingame { .. })
    }

    const fn cable_counts(player_count: usize) -> (usize, usize, usize) {
        let defusing = player_count;
        let bomb = 1;
        let safe = player_count * 5 - defusing - bomb;

        (safe, defusing, bomb)
    }

    fn distribute_cables(&mut self, mut cables: Vec<Cable>) {
        cables.shuffle(&mut thread_rng());

        let cables_per_player = cables.len() / self.players.len();
        for player in self.players.values_mut() {
            player.set_cables(cables.split_off(cables.len() - cables_per_player));
        }
    }

    pub fn start(&mut self) -> Result<(), old_errors::GameStart> {
        if self.players.len() < 4 {
            return Err(old_errors::GameStart::NotEnoughPlayers);
        }
        if !self.players.values().all(WaitingPlayer::ready) {
            return Err(old_errors::GameStart::NotAllPlayersReady);
        }

        let mut teams = match self.players.len() {
            4..=5 => repeated_vec![3 => Team::Sherlock, 2 => Team::Moriarty],
            6 => repeated_vec![4 => Team::Sherlock, 2 => Team::Moriarty],
            7..=8 => repeated_vec![5 => Team::Sherlock, 3 => Team::Moriarty],
            _ => unreachable!(),
        };
        teams.shuffle(&mut thread_rng());

        for player in self.players.values_mut() {
            player.set_team(teams.pop().unwrap());
        }

        let (safe_cables, defusing_cables, bomb) = Self::cable_counts(self.players.len());
        let cables = repeated_vec![safe_cables => Cable::Safe, defusing_cables => Cable::Defusing, bomb => Cable::Bomb];

        self.distribute_cables(cables);

        self.state = GameState::Ingame {
            wire_cutters: *self.players.keys().choose(&mut thread_rng()).unwrap(),
            defusing_remaining: defusing_cables,
            cutted_count: 0,
        };

        Ok(())
    }

    pub fn cut(
        &mut self,
        cutting: PLAYER::ID,
        cutted: PLAYER::ID,
    ) -> Result<(Cable, CutOutcome), old_errors::Cut> {
        if let GameState::Ingame {
            wire_cutters,
            defusing_remaining,
            cutted_count,
        } = &mut self.state
        {
            if cutting != *wire_cutters {
                return Err(old_errors::Cut::DontHaveWireCutter);
            }
            if cutted == cutting {
                return Err(old_errors::Cut::CannotSelfCut);
            }

            let cable = self.players.get_mut(&cutted).unwrap().cut_cable();
            *wire_cutters = cutted;
            match cable {
                Cable::Safe => *cutted_count += 1,
                Cable::Defusing => {
                    *defusing_remaining -= 1;
                    *cutted_count += 1;
                }
                Cable::Bomb => return Ok((cable, CutOutcome::Win(Team::Moriarty))),
            }
            if *defusing_remaining == 0 {
                return Ok((cable, CutOutcome::Win(Team::Sherlock)));
            }

            if *cutted_count == self.players.len() {
                Ok((cable, CutOutcome::RoundEnd))
            } else {
                Ok((cable, CutOutcome::Nothing))
            }
        } else {
            Err(old_errors::Cut::GameNotStarted)
        }
    }

    pub fn next_round(&mut self) -> bool {
        if let GameState::Ingame { cutted_count, .. } = &mut self.state {
            *cutted_count = 0;

            let cables: Vec<Cable> = self
                .players
                .values_mut()
                .flat_map(|p| p.cables().to_owned())
                .collect();

            if cables.len() == self.players.len() {
                return true;
            }

            self.distribute_cables(cables);
        }

        false
    }
}

pub mod old_errors {
    pub enum GameStart {
        NotEnoughPlayers,
        NotAllPlayersReady,
    }

    pub enum Cut {
        GameNotStarted,
        DontHaveWireCutter,
        CannotSelfCut,
    }
}
