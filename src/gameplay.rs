use rand::{
    seq::{IteratorRandom, SliceRandom},
    thread_rng,
};
use rocket::serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt::Debug, hash::Hash};

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
    type ID: Eq + Hash + Clone + Copy + Debug;

    fn id(&self) -> Self::ID;
    fn name(&self) -> &str;
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

pub trait PlayingPlayer: Player {
    fn new<T: WaitingPlayer<ID = Self::ID>>(player: &T, team: Team) -> Self;
    fn connected(&self) -> bool;
    fn team(&self) -> Team;

    fn cables(&self) -> &[Cable];
    fn set_cables(&mut self, cables: Vec<Cable>);
    fn cut_cable(&mut self) -> Cable;
}

#[derive(Debug)]
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

    pub fn may_start(&self) -> bool {
        self.players.len() >= 4 && self.players.values().all(WaitingPlayer::ready)
    }

    pub fn start<T: PlayingPlayer<ID = PLAYER::ID>>(&self) -> Game<T> {
        Game::new(self.name.clone(), &self.players)
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

pub struct Game<PLAYER: PlayingPlayer> {
    name: String,
    players: HashMap<PLAYER::ID, PLAYER>,
    pub wire_cutters: PLAYER::ID,
    defusing_remaining: usize,
    cutted_count: usize,
}

impl<PLAYER: PlayingPlayer> Game<PLAYER> {
    const fn cables_count(player_count: usize) -> (usize, usize, usize) {
        let defusing = player_count;
        let bomb = 1;
        let safe = player_count * 5 - defusing - bomb;

        (safe, defusing, bomb)
    }

    pub fn new<T: WaitingPlayer<ID = PLAYER::ID>>(
        name: String,
        players: &HashMap<T::ID, T>,
    ) -> Self {
        let mut teams = match players.len() {
            4..=5 => repeated_vec![3 => Team::Sherlock, 2 => Team::Moriarty],
            6 => repeated_vec![4 => Team::Sherlock, 2 => Team::Moriarty],
            7..=8 => repeated_vec![5 => Team::Sherlock, 3 => Team::Moriarty],
            _ => unreachable!(),
        };
        teams.shuffle(&mut thread_rng());

        let players: HashMap<_, _> = players
            .iter()
            .zip(teams)
            .map(|((id, player), team)| (*id, PLAYER::new(player, team)))
            .collect();

        let (safe_cables, defusing_cables, bomb) = Self::cables_count(players.len());
        let cables = repeated_vec![safe_cables => Cable::Safe, defusing_cables => Cable::Defusing, bomb => Cable::Bomb];

        let wire_cutters = *players.keys().choose(&mut thread_rng()).unwrap();
        let mut new = Self {
            name,
            players,
            wire_cutters,
            defusing_remaining: defusing_cables,
            cutted_count: 0,
        };

        new.distribute_cables(cables);

        new
    }

    fn distribute_cables(&mut self, mut cables: Vec<Cable>) {
        cables.shuffle(&mut thread_rng());

        let cables_per_player = cables.len() / self.players.len();
        for player in self.players.values_mut() {
            player.set_cables(cables.split_off(cables.len() - cables_per_player));
        }
    }

    pub fn cut(
        &mut self,
        cutting: PLAYER::ID,
        cutted: PLAYER::ID,
    ) -> Result<(Cable, CutOutcome), errors::Cut> {
        if cutting != self.wire_cutters {
            return Err(errors::Cut::DontHaveWireCutter);
        }
        if cutted == cutting {
            return Err(errors::Cut::CannotSelfCut);
        }

        let cable = self.players.get_mut(&cutted).unwrap().cut_cable();
        self.wire_cutters = cutted;
        match cable {
            Cable::Safe => self.cutted_count += 1,
            Cable::Defusing => {
                self.defusing_remaining -= 1;
                self.cutted_count += 1;
            }
            Cable::Bomb => return Ok((cable, CutOutcome::Win(Team::Moriarty))),
        }
        if self.defusing_remaining == 0 {
            return Ok((cable, CutOutcome::Win(Team::Sherlock)));
        }

        if self.cutted_count == self.players.len() {
            Ok((cable, CutOutcome::RoundEnd))
        } else {
            Ok((cable, CutOutcome::Nothing))
        }
    }

    pub fn next_round(&mut self) -> bool {
        self.cutted_count = 0;

        let cables: Vec<Cable> = self
            .players
            .values_mut()
            .flat_map(|p| p.cables().to_owned())
            .collect();

        if cables.len() == self.players.len() {
            return true;
        }

        self.distribute_cables(cables);

        false
    }
}

impl<PLAYER: PlayingPlayer> Room<PLAYER> for Game<PLAYER> {
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

pub enum CutOutcome {
    Win(Team),
    RoundEnd,
    Nothing,
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

    #[derive(Error, Debug, Clone, Copy)]
    pub enum Cut {
        #[error("you don't have the wire cutter")]
        DontHaveWireCutter,
        #[error("you can't cut one of your own card")]
        CannotSelfCut,
    }
}
