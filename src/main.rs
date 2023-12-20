#![allow(clippy::option_if_let_else, clippy::no_effect_underscore_binding)]

use rocket::{
    fs::{relative, FileServer},
    get, launch,
    response::Redirect,
    routes,
};

mod common;
mod game;
mod gameplay;
mod lobby;

use common::GlobalState;

// TODO: auto-delete created game / lobby if nobody joins
// TODO: delete lobby when it goes empty
// TODO: delete game if everyone disconnects (after 1 min to let people time to reconnect)

#[get("/")]
fn index() -> Redirect {
    Redirect::to("/gameMenu.html")
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .manage(GlobalState::new())
        .mount("/", FileServer::from(relative!("static")))
        .mount("/", routes![index])
        .mount("/", game::routes())
        .mount("/", lobby::routes())
}
